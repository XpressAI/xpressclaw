use std::collections::HashMap;
use std::sync::Arc;

use chrono::{NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{error, info, warn};

use crate::agents::registry::AgentRegistry;
use crate::conversations::{ConversationManager, ConversationMessage, SendMessage};
use crate::db::Database;
use crate::error::{Error, Result};
use crate::tasks::board::{CreateTask, TaskBoard};
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::TaskQueue;

use super::condition;
use super::context;
use super::definition::{
    parse_wait_duration, Step, WorkflowDefinition, WorkflowInputType, WorkflowTrigger,
};
use super::instance::{InstanceManager, StepExecution, WorkflowInstance, WorkflowInstanceScope};
use super::manager::WorkflowManager;
use super::waits::{activity_cursor_from_parts, validate_resource, WaitState};

/// Maximum number of times a single step can be re-executed within one
/// workflow instance. Prevents infinite cycles.
const MAX_CYCLES: i32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoopState {
    parent_execution_id: String,
    step_id: String,
    flow_name: String,
    step_index: usize,
    items: Vec<Value>,
    item_index: usize,
    body_index: usize,
    as_var: String,
    /// The body execution currently owned by this cursor. Older persisted
    /// loop states omit it and are recovered from the running execution.
    #[serde(default)]
    active_execution_id: Option<String>,
}

struct PreparedTaskStep {
    task_id: String,
    agent_id: String,
}

/// The workflow runtime engine.
///
/// Manages the lifecycle of workflow instances: starting them from triggers,
/// advancing steps when tasks complete, evaluating conditions, and
/// recovering after crashes.
pub struct WorkflowEngine {
    db: Arc<Database>,
    manager: WorkflowManager,
    instances: InstanceManager,
}

#[derive(Debug, Clone, Default)]
pub struct WorkflowContext {
    pub project_id: Option<String>,
    pub conversation_id: Option<String>,
}

impl WorkflowEngine {
    pub fn new(db: Arc<Database>) -> Self {
        let manager = WorkflowManager::new(db.clone());
        let instances = InstanceManager::new(db.clone());
        Self {
            db,
            manager,
            instances,
        }
    }

    /// Start a new workflow instance from a trigger event.
    ///
    /// Returns the instance ID.
    pub fn start_instance(&self, workflow_id: &str, trigger_data: Value) -> Result<String> {
        self.start_instance_in_context(workflow_id, trigger_data, WorkflowContext::default())
    }

    pub fn start_instance_in_context(
        &self,
        workflow_id: &str,
        trigger_data: Value,
        workflow_context: WorkflowContext,
    ) -> Result<String> {
        self.start_instance_in_context_inner(workflow_id, trigger_data, workflow_context, None)
    }

    pub fn start_instance_in_context_for_conversation_agent(
        &self,
        workflow_id: &str,
        trigger_data: Value,
        workflow_context: WorkflowContext,
        creator_agent_id: &str,
    ) -> Result<String> {
        self.start_instance_in_context_inner(
            workflow_id,
            trigger_data,
            workflow_context,
            Some(creator_agent_id),
        )
    }

    /// Start a Conversation-scoped workflow and publish its durable lifecycle
    /// message in the same write transaction as the workflow instance. The
    /// first workflow step is not dispatched until both records are durable.
    pub fn start_instance_in_context_with_conversation_message(
        &self,
        workflow_id: &str,
        trigger_data: Value,
        workflow_context: WorkflowContext,
        creator_agent_id: Option<&str>,
        message: &SendMessage,
    ) -> Result<(String, ConversationMessage)> {
        let conversation_id = workflow_context.conversation_id.clone().ok_or_else(|| {
            Error::Conversation("workflow message requires a conversation".into())
        })?;
        self.start_instance_in_context_inner_with(
            workflow_id,
            trigger_data,
            workflow_context,
            creator_agent_id,
            |transaction, instance_id| {
                let metadata = serde_json::json!({
                    "workflow_id": workflow_id,
                    "instance_id": instance_id,
                });
                ConversationManager::insert_structured_message(
                    transaction,
                    &conversation_id,
                    message,
                    None,
                    Some(&metadata),
                    &[],
                )
                .map(|(message, _)| message)
            },
        )
    }

    fn start_instance_in_context_inner(
        &self,
        workflow_id: &str,
        trigger_data: Value,
        workflow_context: WorkflowContext,
        creator_agent_id: Option<&str>,
    ) -> Result<String> {
        self.start_instance_in_context_inner_with(
            workflow_id,
            trigger_data,
            workflow_context,
            creator_agent_id,
            |_, _| Ok(()),
        )
        .map(|(instance_id, ())| instance_id)
    }

    fn start_instance_in_context_inner_with<T, F>(
        &self,
        workflow_id: &str,
        trigger_data: Value,
        mut workflow_context: WorkflowContext,
        creator_agent_id: Option<&str>,
        after_instance_insert: F,
    ) -> Result<(String, T)>
    where
        F: FnOnce(&rusqlite::Transaction<'_>, &str) -> Result<T>,
    {
        let record = self.manager.get(workflow_id)?;
        let definition = WorkflowDefinition::parse(&record.yaml_content)?;
        let trigger_data = definition.resolve_inputs(&trigger_data)?;
        if let Some(conversation_id) = workflow_context.conversation_id.as_deref() {
            let conversation_project = self.db.with_conn(|conn| {
                conn.query_row(
                    "SELECT project_id FROM conversations WHERE id = ?1",
                    [conversation_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|_| Error::Workflow(format!("conversation '{conversation_id}' not found")))
            })?;
            if let (Some(requested), Some(actual)) = (
                workflow_context.project_id.as_deref(),
                conversation_project.as_deref(),
            ) {
                if requested != actual {
                    return Err(Error::Workflow(format!(
                        "conversation '{conversation_id}' belongs to project '{actual}', not '{requested}'"
                    )));
                }
            }
            if workflow_context.project_id.is_none() {
                workflow_context.project_id = conversation_project;
            }
        }
        if let Some(project_id) = workflow_context.project_id.as_deref() {
            let exists = self.db.with_conn(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
                    [project_id],
                    |row| row.get::<_, bool>(0),
                )
            })?;
            if !exists {
                return Err(Error::Workflow(format!("project '{project_id}' not found")));
            }
        }
        let registry = AgentRegistry::new(self.db.clone());
        let mut workflow_agent_inputs = Vec::new();
        for (name, input) in &definition.inputs {
            if input.input_type != WorkflowInputType::Agent {
                continue;
            }
            if let Some(agent_id) = trigger_data.get(name).and_then(Value::as_str) {
                workflow_agent_inputs.push((name.clone(), agent_id.to_string()));
                let agent = registry.get(agent_id).map_err(|_| {
                    Error::Workflow(format!(
                        "workflow agent input '{name}' references unknown agent '{agent_id}'"
                    ))
                })?;
                if let Some(project_id) = workflow_context.project_id.as_deref() {
                    if agent.project_id.as_deref() != Some(project_id) {
                        return Err(Error::Workflow(format!(
                            "workflow agent input '{name}' references Agent '{agent_id}' outside project '{project_id}'"
                        )));
                    }
                }
            }
        }

        let trigger_json = serde_json::to_string(&trigger_data)
            .map_err(|e| Error::Workflow(format!("failed to serialize trigger data: {e}")))?;

        // Serialize global variables for the variable store
        let vars_json = if definition.variables.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&definition.variables)
                    .map_err(|e| Error::Workflow(format!("failed to serialize variables: {e}")))?,
            )
        };

        let (instance, after_instance_insert) = self
            .instances
            .create_instance_with_definition_in_context_and_then(
                workflow_id,
                Some(&trigger_json),
                vars_json.as_deref(),
                Some(&record.yaml_content),
                WorkflowInstanceScope {
                    project_id: workflow_context.project_id.as_deref(),
                    conversation_id: workflow_context.conversation_id.as_deref(),
                    creator_agent_id,
                    workflow_agent_inputs: &workflow_agent_inputs,
                },
                after_instance_insert,
            )?;

        info!(
            workflow_id,
            instance_id = instance.id.as_str(),
            "started workflow instance"
        );

        // Determine starting flow — default to "main"
        let start_flow = if definition.flows.contains_key("main") {
            "main".to_string()
        } else {
            // Pick the first flow
            match definition.flow_names().first() {
                Some(name) => name.to_string(),
                None => {
                    self.instances.update_status(
                        &instance.id,
                        "failed",
                        Some("no flows in workflow"),
                    )?;
                    return Err(Error::Workflow("workflow has no flows".into()));
                }
            }
        };

        // Load variable store
        let var_store = self.load_variable_store(&instance.id)?;

        self.execute_step(
            &instance.id,
            &start_flow,
            0,
            &definition,
            &trigger_data,
            &var_store,
        )?;

        Ok((instance.id, after_instance_insert))
    }

    /// Execute a step at the given position.
    fn execute_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_index: usize,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
    ) -> Result<()> {
        let result = self.execute_step_inner(
            instance_id,
            flow_name,
            step_index,
            definition,
            trigger_data,
            variable_store,
        );
        if let Err(error) = &result {
            if self
                .instances
                .get_instance(instance_id)
                .is_ok_and(|instance| matches!(instance.status.as_str(), "running" | "waiting"))
            {
                let _ =
                    self.instances
                        .update_status(instance_id, "failed", Some(&error.to_string()));
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_step_inner(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_index: usize,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
    ) -> Result<()> {
        let flow = definition
            .flows
            .get(flow_name)
            .ok_or_else(|| Error::Workflow(format!("flow '{flow_name}' not found in workflow")))?;

        if step_index >= flow.steps.len() {
            // Past the end of this flow — workflow is done
            info!(instance_id, flow_name, "flow completed, finishing instance");
            self.instances.complete_instance(instance_id)?;
            return Ok(());
        }

        let step = &flow.steps[step_index];

        // Cycle guard
        let attempts = self
            .instances
            .get_step_attempt_count(instance_id, &step.id)?;
        if attempts >= MAX_CYCLES {
            let msg = format!("step '{}' exceeded max cycles ({})", step.id, MAX_CYCLES);
            error!(instance_id, step_id = step.id.as_str(), "{}", msg);
            self.instances
                .update_status(instance_id, "failed", Some(&msg))?;
            return Err(Error::Workflow(msg));
        }

        // Update current position
        self.instances
            .set_current_position(instance_id, flow_name, step_index as i32)?;

        // Build context
        let ctx = context::build_context(trigger_data, &definition.variables, variable_store);
        let ctx_json = serde_json::to_string(&ctx).unwrap_or_else(|_| "{}".to_string());

        match step.step_type.as_str() {
            "step" => self
                .execute_task_step(instance_id, flow_name, step, &ctx, &ctx_json)
                .map(|_| ()),
            "wait" => self.execute_wait_step(instance_id, flow_name, step, &ctx),
            "sink" => self.execute_sink_step(
                instance_id,
                flow_name,
                step_index,
                step,
                &ctx,
                &ctx_json,
                definition,
                trigger_data,
                variable_store,
            ),
            "when" => self.execute_when_step(
                instance_id,
                flow_name,
                step_index,
                step,
                definition,
                trigger_data,
                variable_store,
                &ctx,
            ),
            "loop" => self.execute_loop_step(
                instance_id,
                flow_name,
                step_index,
                step,
                definition,
                trigger_data,
                variable_store,
                &ctx,
            ),
            "jump" => self.execute_jump_step(
                instance_id,
                step,
                definition,
                trigger_data,
                variable_store,
                &ctx_json,
                flow_name,
            ),
            other => {
                warn!(
                    instance_id,
                    step_id = step.id.as_str(),
                    step_type = other,
                    "unknown step type, treating as task"
                );
                self.execute_task_step(instance_id, flow_name, step, &ctx, &ctx_json)
                    .map(|_| ())
            }
        }
    }

    /// Execute a task step: render prompt, create task, enqueue, wait for completion.
    fn execute_task_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step: &Step,
        ctx: &Value,
        ctx_json: &str,
    ) -> Result<StepExecution> {
        let prepared = self.prepare_task_step(instance_id, flow_name, step, ctx)?;
        let execution = self.instances.create_task_execution(
            instance_id,
            flow_name,
            &step.id,
            Some(ctx_json),
            &prepared.task_id,
        )?;
        self.dispatch_task_step(step, &prepared, &execution)?;
        Ok(execution)
    }

    /// Render and persist a task without making it visible to the dispatcher.
    /// The workflow execution (and loop cursor, where applicable) must own the
    /// task before `dispatch_task_step` is called.
    fn prepare_task_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step: &Step,
        ctx: &Value,
    ) -> Result<PreparedTaskStep> {
        let agent_id = self.resolve_step_agent(step, ctx)?;
        let workflow_context = self.instances.get_instance(instance_id)?;
        if let Some(project_id) = workflow_context.project_id.as_deref() {
            let belongs = self.db.with_conn(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM agents WHERE id = ?1 AND project_id = ?2)",
                    rusqlite::params![agent_id, project_id],
                    |row| row.get::<_, bool>(0),
                )
                .unwrap_or(false)
            });
            if !belongs {
                return Err(Error::Workflow(format!(
                    "workflow step '{}' uses Agent '{}' outside project '{}'",
                    step.id, agent_id, project_id
                )));
            }
        }
        let rendered_prompt = match &step.prompt {
            Some(tmpl) => context::render_template(tmpl, ctx),
            None => format!("Execute workflow step: {}", step.id),
        };

        let rendered_session_config: HashMap<String, Value> = step
            .session_config
            .iter()
            .map(|(key, value)| {
                let value = value
                    .as_str()
                    .map(|value| Value::String(context::render_template(value, ctx)))
                    .unwrap_or_else(|| value.clone());
                (key.clone(), value)
            })
            .collect();

        let rendered_prompt = if let Some(tool) = step
            .mcp_tool
            .as_deref()
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
        {
            let server = step
                .mcp_server
                .as_deref()
                .map(str::trim)
                .filter(|server| !server.is_empty())
                .map(|server| format!(" from the attached '{server}' server"))
                .unwrap_or_default();
            let arguments = step
                .mcp_arguments
                .as_ref()
                .map(|value| render_json_templates(value, ctx))
                .unwrap_or_else(|| serde_json::json!({}));
            let arguments =
                serde_json::to_string_pretty(&arguments).unwrap_or_else(|_| "{}".to_string());
            format!(
                "Call the MCP tool '{tool}'{server} with these arguments before completing this step:\n{arguments}\n\n{rendered_prompt}"
            )
        } else {
            rendered_prompt
        };

        let rendered_prompt = if let Some(command) = step
            .command
            .as_deref()
            .map(str::trim)
            .filter(|command| !command.is_empty())
        {
            let command = context::render_template(command, ctx);
            let command = if command.starts_with('/') {
                command
            } else {
                format!("/{command}")
            };
            if !rendered_prompt.trim().is_empty() {
                format!("{command} {}", rendered_prompt.trim())
            } else {
                command
            }
        } else {
            rendered_prompt
        };

        // If step has declared outputs, append output schema to prompt
        let full_prompt = if let Some(ref outputs) = step.outputs {
            let mut schema_lines = vec![
                String::new(),
                "Respond with a JSON object containing these fields:".to_string(),
            ];
            for (name, schema) in outputs {
                let type_str = schema.output_type.as_deref().unwrap_or("string");
                let desc = schema.description.as_deref().unwrap_or("");
                schema_lines.push(format!("- \"{name}\" ({type_str}): {desc}"));
            }
            format!("{rendered_prompt}\n{}", schema_lines.join("\n"))
        } else {
            rendered_prompt
        };

        let label = step.label.as_deref().unwrap_or(&step.id).to_string();

        let board = TaskBoard::new(self.db.clone());
        let task = board.create(&CreateTask {
            title: label,
            description: Some(full_prompt),
            agent_id: Some(agent_id.clone()),
            parent_task_id: None,
            sop_id: step.procedure.clone(),
            conversation_id: workflow_context.conversation_id.clone(),
            priority: None,
            context: Some(serde_json::json!({
                "origin": "workflow",
                "kind": "workflow",
                "source_id": instance_id,
                "flow": flow_name,
                "step": step.id,
                "session_mode": if step.new_session { "new" } else { "continue" },
                "session_config": rendered_session_config,
                "project_id": workflow_context.project_id,
                "conversation_id": workflow_context.conversation_id,
            })),
        })?;

        Ok(PreparedTaskStep {
            task_id: task.id,
            agent_id,
        })
    }

    fn dispatch_task_step(
        &self,
        step: &Step,
        prepared: &PreparedTaskStep,
        execution: &StepExecution,
    ) -> Result<()> {
        let queue = TaskQueue::new(self.db.clone());
        if let Err(error) = queue.ensure_enqueued(&prepared.task_id, &prepared.agent_id) {
            let _ =
                TaskBoard::new(self.db.clone()).update_status(&prepared.task_id, "cancelled", None);
            let _ = self.instances.update_step_status(
                &execution.id,
                "failed",
                Some(&error.to_string()),
            );
            return Err(Error::Workflow(format!(
                "failed to enqueue workflow step '{}' for agent '{}': {error}",
                step.id, prepared.agent_id
            )));
        }

        info!(
            instance_id = execution.instance_id.as_str(),
            flow_name = execution.flow_name.as_str(),
            step_id = step.id.as_str(),
            task_id = prepared.task_id.as_str(),
            "executing task step"
        );
        Ok(())
    }

    fn resolve_step_agent(&self, step: &Step, ctx: &Value) -> Result<String> {
        let configured = step
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|agent| !agent.is_empty())
            .ok_or_else(|| Error::Workflow(format!("step '{}' has no agent", step.id)))?;
        let rendered = context::render_template(configured, ctx);
        if rendered.starts_with('@') || rendered.contains("{{") || rendered.trim().is_empty() {
            return Err(Error::Workflow(format!(
                "step '{}' could not resolve agent binding '{configured}'",
                step.id
            )));
        }
        Ok(rendered)
    }

    /// Persist an event wait without holding an agent container or task open.
    fn execute_wait_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step: &Step,
        ctx: &Value,
    ) -> Result<()> {
        let agent_id = self.resolve_step_agent(step, ctx)?;
        let event = step.event.as_deref().unwrap_or_default().trim().to_string();
        let resource =
            context::render_template(step.resource.as_deref().unwrap_or_default().trim(), ctx);
        if resource.starts_with('@') || resource.contains("{{") || resource.trim().is_empty() {
            return Err(Error::Workflow(format!(
                "wait step '{}' could not resolve resource '{}'",
                step.id,
                step.resource.as_deref().unwrap_or_default()
            )));
        }
        validate_resource(&event, &resource)?;
        let previous_event = ctx.get(&step.id).and_then(Value::as_object);
        let previous_timestamp = previous_event
            .and_then(|event| event.get("created_at"))
            .and_then(Value::as_str)
            .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
            .map(|timestamp| timestamp.with_timezone(&Utc));
        let after_cursor = previous_event
            .and_then(|event| event.get("cursor"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                let event = previous_event?;
                let kind = event.get("kind")?.as_str()?;
                let id = event.get("id")?;
                Some(activity_cursor_from_parts(kind, id))
            });
        // Begin the first wait at the preceding agent task's start, not when
        // its final response happened to reach the engine. A human can react
        // immediately after the agent marks a PR ready, while the agent is
        // still composing its response. Repeated waits use the prior event
        // cursor instead and do not replay that activity.
        let started_at = previous_timestamp
            .or_else(|| self.initial_wait_boundary(instance_id))
            .unwrap_or_else(|| Utc::now() - chrono::Duration::minutes(5));
        let now = Utc::now();
        let timeout_at = step
            .timeout
            .as_deref()
            .map(parse_wait_duration)
            .transpose()
            .map_err(Error::Workflow)?
            .map(|duration| {
                now.checked_add_signed(duration)
                    .ok_or_else(|| Error::Workflow("wait timeout exceeds timestamp range".into()))
                    .map(|timestamp| timestamp.to_rfc3339())
            })
            .transpose()?;
        let state = WaitState {
            event,
            resource,
            agent_id,
            started_at: started_at.to_rfc3339(),
            after_cursor,
            timeout_at,
            next_poll_at: None,
            poll_interval_seconds: 15,
            last_checked_at: None,
            last_error: None,
        };
        let state_json = serde_json::to_string(&state)
            .map_err(|error| Error::Workflow(format!("failed to persist wait state: {error}")))?;
        let execution = self.instances.create_agent_wait_execution(
            instance_id,
            flow_name,
            &step.id,
            &state.agent_id,
            &state_json,
        )?;
        info!(
            instance_id,
            flow_name,
            step_id = step.id.as_str(),
            execution_id = execution.id.as_str(),
            event = state.event.as_str(),
            resource = state.resource.as_str(),
            "workflow is waiting for an event"
        );
        Ok(())
    }

    fn initial_wait_boundary(&self, instance_id: &str) -> Option<chrono::DateTime<Utc>> {
        let executions = self.instances.list_step_executions(instance_id).ok()?;
        executions
            .iter()
            .rev()
            .find(|execution| execution.task_id.is_some() && execution.status == "completed")
            .and_then(|execution| execution.started_at.as_deref())
            .and_then(parse_database_timestamp)
            .or_else(|| {
                self.instances
                    .get_instance(instance_id)
                    .ok()
                    .and_then(|instance| parse_database_timestamp(&instance.started_at))
            })
    }

    /// Execute a sink step: deliver messages, then advance.
    #[allow(clippy::too_many_arguments)]
    fn execute_sink_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_index: usize,
        step: &Step,
        ctx: &Value,
        ctx_json: &str,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
    ) -> Result<()> {
        let exec = self.instances.create_step_execution(
            instance_id,
            flow_name,
            &step.id,
            Some(ctx_json),
        )?;

        if let Some(ref sinks) = step.sinks {
            for sink in sinks {
                let rendered = match &sink.template {
                    Some(tmpl) => context::render_template(tmpl, ctx),
                    None => format!("Workflow step '{}' completed", step.id),
                };
                crate::connectors::deliver::deliver(
                    &self.db,
                    &sink.connector,
                    &sink.channel,
                    &rendered,
                );
            }
        }

        self.instances
            .update_step_status(&exec.id, "completed", Some("sink delivered"))?;

        info!(
            instance_id,
            flow_name,
            step_id = step.id.as_str(),
            "sink step delivered"
        );

        // Advance to next step
        self.advance_to_next(
            instance_id,
            flow_name,
            step_index,
            definition,
            trigger_data,
            variable_store,
        )
    }

    /// Execute a when (conditional) step: resolve switch var, match arms, branch.
    #[allow(clippy::too_many_arguments)]
    fn execute_when_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_index: usize,
        step: &Step,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
        ctx: &Value,
    ) -> Result<()> {
        let switch_expr = step.switch_var.as_deref().unwrap_or("");
        let resolved_value = context::resolve_variable(switch_expr, ctx)
            .map(|v| match v {
                Value::String(s) => s,
                other => serde_json::to_string(&other).unwrap_or_default(),
            })
            .unwrap_or_default();

        info!(
            instance_id,
            step_id = step.id.as_str(),
            switch_expr,
            resolved = resolved_value.as_str(),
            "evaluating when step"
        );

        // Create execution record for the when step
        let ctx_json = serde_json::to_string(ctx).unwrap_or_else(|_| "{}".to_string());
        let exec = self.instances.create_step_execution(
            instance_id,
            flow_name,
            &step.id,
            Some(&ctx_json),
        )?;
        self.instances
            .update_step_status(&exec.id, "completed", Some(&resolved_value))?;

        let arms = match &step.arms {
            Some(a) => a,
            None => {
                // No arms — just continue to next step
                return self.advance_to_next(
                    instance_id,
                    flow_name,
                    step_index,
                    definition,
                    trigger_data,
                    variable_store,
                );
            }
        };

        // Find matching arm (check non-default first, then default)
        let mut default_arm = None;
        let mut matched_arm = None;

        for arm in arms {
            let match_val = arm.match_value.as_deref().unwrap_or("");
            if match_val == "default" {
                default_arm = Some(arm);
                continue;
            }
            if condition::evaluate_match(match_val, &resolved_value) {
                matched_arm = Some(arm);
                break;
            }
        }

        let arm = matched_arm.or(default_arm);

        match arm {
            Some(a) => {
                if a.continue_flow.unwrap_or(false) {
                    // Continue to next step in current flow
                    self.advance_to_next(
                        instance_id,
                        flow_name,
                        step_index,
                        definition,
                        trigger_data,
                        variable_store,
                    )
                } else if let Some(ref goto) = a.goto {
                    self.resolve_goto(
                        instance_id,
                        goto,
                        flow_name,
                        definition,
                        trigger_data,
                        variable_store,
                    )
                } else {
                    // No action — continue to next step
                    self.advance_to_next(
                        instance_id,
                        flow_name,
                        step_index,
                        definition,
                        trigger_data,
                        variable_store,
                    )
                }
            }
            None => {
                // No matching arm — continue to next step
                info!(
                    instance_id,
                    step_id = step.id.as_str(),
                    "no matching arm in when, continuing"
                );
                self.advance_to_next(
                    instance_id,
                    flow_name,
                    step_index,
                    definition,
                    trigger_data,
                    variable_store,
                )
            }
        }
    }

    /// Execute a loop step serially. The complete cursor and item collection
    /// are persisted before each asynchronous task, allowing task completion
    /// and process recovery to resume at the exact body position.
    #[allow(clippy::too_many_arguments)]
    fn execute_loop_step(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_index: usize,
        step: &Step,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
        ctx: &Value,
    ) -> Result<()> {
        let items_value = context::resolve_variable(step.over.as_deref().unwrap_or(""), ctx)
            .unwrap_or(Value::Array(vec![]));
        let items = match items_value {
            Value::Array(items) => items,
            item => vec![item],
        };
        let ctx_json = serde_json::to_string(ctx).unwrap_or_else(|_| "{}".to_string());
        let execution = self.instances.create_step_execution(
            instance_id,
            flow_name,
            &step.id,
            Some(&ctx_json),
        )?;
        self.instances.mark_step_running(&execution.id)?;
        let state = LoopState {
            parent_execution_id: execution.id,
            step_id: step.id.clone(),
            flow_name: flow_name.to_string(),
            step_index,
            items,
            item_index: 0,
            body_index: 0,
            as_var: step.as_var.clone().unwrap_or_else(|| "item".into()),
            active_execution_id: None,
        };
        info!(
            instance_id,
            step_id = step.id,
            item_count = state.items.len(),
            "executing durable loop step"
        );
        self.continue_loop(
            instance_id,
            state,
            definition,
            trigger_data,
            variable_store.clone(),
        )
    }

    fn continue_loop(
        &self,
        instance_id: &str,
        mut state: LoopState,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        mut variables: HashMap<String, Value>,
    ) -> Result<()> {
        let loop_step = definition
            .find_step(&state.flow_name, &state.step_id)
            .ok_or_else(|| Error::Workflow(format!("loop step '{}' not found", state.step_id)))?;
        let body = loop_step.body.as_deref().unwrap_or_default();

        if state.items.is_empty() || body.is_empty() {
            return self.complete_loop(instance_id, &state, definition, trigger_data, &variables);
        }
        if state.body_index >= body.len() {
            state.item_index += 1;
            state.body_index = 0;
        }
        if state.item_index >= state.items.len() {
            variables.remove(&state.as_var);
            return self.complete_loop(instance_id, &state, definition, trigger_data, &variables);
        }

        variables.insert(state.as_var.clone(), state.items[state.item_index].clone());
        self.save_variable_store(instance_id, &variables)?;
        self.instances.update_loop_state(
            instance_id,
            Some(
                &serde_json::to_string(&state)
                    .map_err(|error| Error::Workflow(format!("failed to persist loop: {error}")))?,
            ),
        )?;

        let body_step = &body[state.body_index];
        let body_context = context::build_context(trigger_data, &definition.variables, &variables);
        let body_context_json =
            serde_json::to_string(&body_context).unwrap_or_else(|_| "{}".to_string());
        let prepared =
            self.prepare_task_step(instance_id, &state.flow_name, body_step, &body_context)?;
        let flow_name = state.flow_name.clone();
        let execution = self.instances.create_loop_task_execution(
            instance_id,
            &flow_name,
            &body_step.id,
            Some(&body_context_json),
            &prepared.task_id,
            |execution_id| {
                state.active_execution_id = Some(execution_id.to_string());
                serde_json::to_string(&state)
                    .map_err(|error| Error::Workflow(format!("failed to persist loop: {error}")))
            },
        )?;
        self.dispatch_task_step(body_step, &prepared, &execution)?;
        Ok(())
    }

    fn complete_loop(
        &self,
        instance_id: &str,
        state: &LoopState,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variables: &HashMap<String, Value>,
    ) -> Result<()> {
        self.save_variable_store(instance_id, variables)?;
        self.instances.update_loop_state(instance_id, None)?;
        self.instances.update_step_status(
            &state.parent_execution_id,
            "completed",
            Some(&serde_json::json!({ "iterations": state.items.len() }).to_string()),
        )?;
        self.advance_to_next(
            instance_id,
            &state.flow_name,
            state.step_index,
            definition,
            trigger_data,
            variables,
        )
    }

    /// Execute a jump step: parse target and switch flow/step.
    #[allow(clippy::too_many_arguments)]
    fn execute_jump_step(
        &self,
        instance_id: &str,
        step: &Step,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
        ctx_json: &str,
        flow_name: &str,
    ) -> Result<()> {
        let target = step.target.as_deref().unwrap_or("");

        let exec = self.instances.create_step_execution(
            instance_id,
            flow_name,
            &step.id,
            Some(ctx_json),
        )?;
        self.instances
            .update_step_status(&exec.id, "completed", Some(target))?;

        info!(
            instance_id,
            step_id = step.id.as_str(),
            target,
            "executing jump step"
        );

        self.resolve_goto(
            instance_id,
            target,
            flow_name,
            definition,
            trigger_data,
            variable_store,
        )
    }

    /// Resolve a goto/jump target and execute accordingly.
    ///
    /// Formats:
    /// - `"step <id>"` — find step index in current flow
    /// - `"flow <name>"` — switch to that flow at step 0
    /// - `"flow <name> step <id>"` — switch to that flow at that step
    /// - `"workflow <id>"` — start a new workflow instance
    fn resolve_goto(
        &self,
        instance_id: &str,
        target: &str,
        current_flow: &str,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
    ) -> Result<()> {
        let parts: Vec<&str> = target.split_whitespace().collect();
        match parts.as_slice() {
            ["step", step_id] => {
                let idx = definition
                    .step_index(current_flow, step_id)
                    .ok_or_else(|| {
                        Error::Workflow(format!(
                            "goto target step '{step_id}' not found in flow '{current_flow}'"
                        ))
                    })?;
                self.execute_step(
                    instance_id,
                    current_flow,
                    idx,
                    definition,
                    trigger_data,
                    variable_store,
                )
            }
            ["flow", flow_name] => {
                // Empty terminal flows complete before `execute_step_inner`
                // reaches a concrete step, so persist the transition here as
                // well as in the normal non-empty path.
                self.instances
                    .set_current_position(instance_id, flow_name, 0)?;
                self.execute_step(
                    instance_id,
                    flow_name,
                    0,
                    definition,
                    trigger_data,
                    variable_store,
                )
            }
            ["flow", flow_name, "step", step_id] => {
                let idx = definition.step_index(flow_name, step_id).ok_or_else(|| {
                    Error::Workflow(format!(
                        "goto target step '{step_id}' not found in flow '{flow_name}'"
                    ))
                })?;
                self.execute_step(
                    instance_id,
                    flow_name,
                    idx,
                    definition,
                    trigger_data,
                    variable_store,
                )
            }
            ["workflow", workflow_name] => {
                // Start a new workflow instance — trigger data is carried over
                info!(
                    instance_id,
                    target_workflow = *workflow_name,
                    "jumping to new workflow"
                );
                // Find workflow by name
                let workflows = self.manager.list()?;
                let target_wf = workflows.iter().find(|w| w.name == *workflow_name);
                match target_wf {
                    Some(wf) => {
                        self.start_instance(&wf.id, trigger_data.clone())?;
                    }
                    None => {
                        warn!(
                            workflow_name = *workflow_name,
                            "jump target workflow not found"
                        );
                    }
                }
                // Complete the current instance
                self.instances.complete_instance(instance_id)?;
                Ok(())
            }
            _ => Err(Error::Workflow(format!("invalid goto target: '{target}'"))),
        }
    }

    /// Advance to the next step in the current flow.
    #[allow(clippy::too_many_arguments)]
    fn advance_to_next(
        &self,
        instance_id: &str,
        flow_name: &str,
        current_step_index: usize,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variable_store: &HashMap<String, Value>,
    ) -> Result<()> {
        let next_index = current_step_index + 1;
        self.execute_step(
            instance_id,
            flow_name,
            next_index,
            definition,
            trigger_data,
            variable_store,
        )
    }

    /// Called by the task dispatcher after a task completes.
    pub fn on_task_completed(
        &self,
        task_id: &str,
        task_status: &str,
        task_output: &str,
    ) -> Result<()> {
        let exec = match self.instances.find_execution_by_task(task_id)? {
            Some(e) => e,
            None => return Ok(()), // Not part of a workflow
        };

        // Map task status to step status
        let step_status = match task_status {
            "completed" => "completed",
            "cancelled" => "failed",
            _ => "failed",
        };

        self.instances
            .update_step_status(&exec.id, step_status, Some(task_output))?;

        // Load workflow definition
        let instance = self.instances.get_instance(&exec.instance_id)?;
        if instance.status != "running" {
            return Ok(()); // Instance already finished
        }

        let definition = self.definition_for_instance(&instance)?;

        // Gather trigger data
        let trigger_data: Value = instance
            .trigger_data
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);

        // Load and update variable store
        let mut var_store = self.load_variable_store(&exec.instance_id)?;

        // Try to parse output as JSON and store under step_id
        let output_value = parse_task_output(task_output);

        // If step has declared outputs, extract those fields
        if let Some(step) = definition.find_step(&exec.flow_name, &exec.step_id) {
            if let Some(ref outputs) = step.outputs {
                if let Value::Object(ref map) = output_value {
                    let mut extracted = serde_json::Map::new();
                    for key in outputs.keys() {
                        if let Some(v) = map.get(key) {
                            extracted.insert(key.clone(), v.clone());
                        }
                    }
                    var_store.insert(exec.step_id.clone(), Value::Object(extracted));
                } else {
                    // Not a JSON object — store raw output
                    var_store.insert(
                        exec.step_id.clone(),
                        serde_json::json!({ "output": output_value }),
                    );
                }
            } else {
                // No declared outputs — store the JSON as-is if it's an object,
                // otherwise wrap it so @step.output works
                match &output_value {
                    Value::Object(_) => {
                        var_store.insert(exec.step_id.clone(), output_value.clone());
                    }
                    _ => {
                        var_store.insert(
                            exec.step_id.clone(),
                            serde_json::json!({ "output": output_value }),
                        );
                    }
                }
            }
        } else {
            match &output_value {
                Value::Object(_) => {
                    var_store.insert(exec.step_id.clone(), output_value.clone());
                }
                _ => {
                    var_store.insert(
                        exec.step_id.clone(),
                        serde_json::json!({ "output": output_value }),
                    );
                }
            }
        }

        self.save_variable_store(&exec.instance_id, &var_store)?;

        if step_status == "failed" {
            if let Some(loop_state) = instance
                .loop_state
                .as_deref()
                .and_then(|state| serde_json::from_str::<LoopState>(state).ok())
            {
                self.instances.update_step_status(
                    &loop_state.parent_execution_id,
                    "failed",
                    Some(task_output),
                )?;
                self.instances.update_loop_state(&exec.instance_id, None)?;
            }
            // Check if there's an on_error flow
            if definition.flows.contains_key("on_error") {
                info!(
                    instance_id = exec.instance_id.as_str(),
                    step_id = exec.step_id.as_str(),
                    "step failed, jumping to on_error flow"
                );
                return self.execute_step(
                    &exec.instance_id,
                    "on_error",
                    0,
                    &definition,
                    &trigger_data,
                    &var_store,
                );
            }

            // No error handler — fail the instance
            self.instances.update_status(
                &exec.instance_id,
                "failed",
                Some(&format!("step '{}' failed: {}", exec.step_id, task_output)),
            )?;
            return Ok(());
        }

        if let Some(loop_state) = instance
            .loop_state
            .as_deref()
            .and_then(|state| serde_json::from_str::<LoopState>(state).ok())
        {
            let loop_step = definition
                .find_step(&loop_state.flow_name, &loop_state.step_id)
                .and_then(|step| step.body.as_deref());
            if loop_step
                .and_then(|body| body.get(loop_state.body_index))
                .is_some_and(|body_step| body_step.id == exec.step_id)
                && loop_state
                    .active_execution_id
                    .as_deref()
                    .is_none_or(|active| active == exec.id.as_str())
            {
                let mut next_state = loop_state;
                next_state.body_index += 1;
                next_state.active_execution_id = None;
                self.instances.update_loop_state(
                    &exec.instance_id,
                    Some(&serde_json::to_string(&next_state).map_err(|error| {
                        Error::Workflow(format!("failed to persist loop: {error}"))
                    })?),
                )?;
                return self.continue_loop(
                    &exec.instance_id,
                    next_state,
                    &definition,
                    &trigger_data,
                    var_store,
                );
            }
        }

        // Find current step index and advance
        let step_index = definition
            .step_index(&exec.flow_name, &exec.step_id)
            .unwrap_or(0);

        self.advance_to_next(
            &exec.instance_id,
            &exec.flow_name,
            step_index,
            &definition,
            &trigger_data,
            &var_store,
        )
    }

    /// Resume one durable wait with a matched event payload. The event is
    /// claimed atomically before workflow state advances, so overlapping poll
    /// passes cannot create duplicate downstream tasks.
    pub fn resume_wait_execution(&self, execution_id: &str, payload: Value) -> Result<()> {
        let output = serde_json::to_string(&payload)
            .map_err(|error| Error::Workflow(format!("failed to serialize wait event: {error}")))?;
        if !self.instances.claim_wait(execution_id, &output)? {
            return Ok(());
        }
        self.finish_wait_execution(execution_id)
    }

    /// Take the configured timeout branch for a durable wait, or fail the
    /// instance when the workflow did not define one.
    pub fn timeout_wait_execution(&self, execution_id: &str) -> Result<()> {
        let output = serde_json::json!({ "kind": "timeout" }).to_string();
        if !self.instances.claim_wait(execution_id, &output)? {
            return Ok(());
        }
        self.finish_wait_execution(execution_id)
    }

    fn finish_wait_execution(&self, execution_id: &str) -> Result<()> {
        let execution = self.instances.get_step_execution(execution_id)?;
        if execution.status != "resuming" {
            return Ok(());
        }
        let instance = self.instances.get_instance(&execution.instance_id)?;
        if !matches!(instance.status.as_str(), "running" | "waiting") {
            return Ok(());
        }
        self.instances
            .set_active_status(&execution.instance_id, "running")?;
        let definition = self.definition_for_instance(&instance)?;
        let step = definition
            .find_step(&execution.flow_name, &execution.step_id)
            .ok_or_else(|| {
                Error::Workflow(format!(
                    "wait step '{}' no longer exists in workflow",
                    execution.step_id
                ))
            })?;
        if step.step_type != "wait" {
            return Err(Error::Workflow(format!(
                "execution '{}' is not a wait step",
                execution.id
            )));
        }
        let payload = execution
            .output
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or(Value::Null);
        let trigger_data: Value = instance
            .trigger_data
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or(Value::Null);
        let mut variables = self.load_variable_store(&execution.instance_id)?;
        variables.insert(execution.step_id.clone(), payload.clone());
        self.save_variable_store(&execution.instance_id, &variables)?;

        if payload.get("kind").and_then(Value::as_str) == Some("timeout") {
            self.instances.update_step_status(
                execution_id,
                if step.on_timeout.is_some() {
                    "completed"
                } else {
                    "failed"
                },
                execution.output.as_deref(),
            )?;
            if step.on_timeout.is_some() {
                return self.advance_completed_wait(
                    &execution,
                    step,
                    &definition,
                    &trigger_data,
                    &variables,
                );
            }
            self.instances.update_status(
                &execution.instance_id,
                "failed",
                Some(&format!("wait step '{}' timed out", execution.step_id)),
            )?;
            return Ok(());
        }

        self.instances.update_step_status(
            execution_id,
            "completed",
            execution.output.as_deref(),
        )?;
        self.advance_completed_wait(&execution, step, &definition, &trigger_data, &variables)
    }

    /// Advance a wait that is already durably completed. Recovery calls this
    /// when a process stopped after recording the event but before creating
    /// the next task or following the timeout branch.
    fn advance_completed_wait(
        &self,
        execution: &StepExecution,
        step: &Step,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        variables: &HashMap<String, Value>,
    ) -> Result<()> {
        let payload = execution
            .output
            .as_deref()
            .and_then(|value| serde_json::from_str::<Value>(value).ok())
            .unwrap_or(Value::Null);
        if payload.get("kind").and_then(Value::as_str) == Some("timeout") {
            let target = step.on_timeout.as_deref().ok_or_else(|| {
                Error::Workflow(format!(
                    "wait step '{}' timed out without a timeout target",
                    execution.step_id
                ))
            })?;
            return self.resolve_goto(
                &execution.instance_id,
                target,
                &execution.flow_name,
                definition,
                trigger_data,
                variables,
            );
        }
        let step_index = definition
            .step_index(&execution.flow_name, &execution.step_id)
            .ok_or_else(|| {
                Error::Workflow(format!(
                    "wait step '{}' must be a top-level flow step",
                    execution.step_id
                ))
            })?;
        self.advance_to_next(
            &execution.instance_id,
            &execution.flow_name,
            step_index,
            definition,
            trigger_data,
            variables,
        )
    }

    /// Find a step execution by task ID.
    pub fn find_execution_by_task(&self, task_id: &str) -> Result<Option<StepExecution>> {
        self.instances.find_execution_by_task(task_id)
    }

    fn definition_for_instance(&self, instance: &WorkflowInstance) -> Result<WorkflowDefinition> {
        match instance.definition_yaml.as_deref() {
            Some(snapshot) => WorkflowDefinition::parse(snapshot),
            None => {
                let record = self.manager.get(&instance.workflow_id)?;
                WorkflowDefinition::parse(&record.yaml_content)
            }
        }
    }

    fn completed_task_output(&self, task_id: &str) -> String {
        TaskConversation::new(self.db.clone())
            .get_messages(task_id)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .find(|message| message.role == "assistant")
            .map(|message| message.content)
            .unwrap_or_default()
    }

    /// Recover running workflow instances after a restart.
    pub fn recover(&self) -> Result<()> {
        let running = self.instances.list_running_instances()?;
        if running.is_empty() {
            return Ok(());
        }

        info!(count = running.len(), "recovering workflow instances");

        let board = TaskBoard::new(self.db.clone());

        for instance in &running {
            // Find the latest running step execution
            let execs = self.instances.list_step_executions(&instance.id)?;
            if let Some(execution) = execs
                .iter()
                .rfind(|execution| execution.status == "resuming")
            {
                if let Err(error) = self.finish_wait_execution(&execution.id) {
                    error!(instance_id = instance.id, execution_id = execution.id, error = %error, "failed to recover workflow wait");
                }
                continue;
            }
            let current_exec = execs.iter().rfind(|e| e.status == "running");

            if let Some(exec) = current_exec {
                if let Some(ref task_id) = exec.task_id {
                    match board.get(task_id) {
                        Ok(task) => {
                            let status = task.status.as_str();
                            if status == "completed" || status == "cancelled" {
                                info!(
                                    instance_id = instance.id.as_str(),
                                    task_id = task_id.as_str(),
                                    task_status = status,
                                    "recovering completed task for workflow"
                                );
                                let output = self.completed_task_output(task_id);
                                if let Err(e) = self.on_task_completed(task_id, status, &output) {
                                    error!(
                                        instance_id = instance.id.as_str(),
                                        task_id = task_id.as_str(),
                                        error = %e,
                                        "failed to recover workflow task"
                                    );
                                }
                            } else if status == "pending" {
                                match task.agent_id.as_deref() {
                                    Some(agent_id) => {
                                        match TaskQueue::new(self.db.clone())
                                            .ensure_enqueued(task_id, agent_id)
                                        {
                                            Ok(Some(_)) => info!(
                                                instance_id = instance.id.as_str(),
                                                execution_id = exec.id.as_str(),
                                                task_id = task_id.as_str(),
                                                "recovered workflow task dispatch"
                                            ),
                                            Ok(None) => {}
                                            Err(error) => error!(
                                                instance_id = instance.id.as_str(),
                                                execution_id = exec.id.as_str(),
                                                task_id = task_id.as_str(),
                                                error = %error,
                                                "failed to recover workflow task dispatch"
                                            ),
                                        }
                                    }
                                    None => error!(
                                        instance_id = instance.id.as_str(),
                                        execution_id = exec.id.as_str(),
                                        task_id = task_id.as_str(),
                                        "cannot recover workflow task without an agent"
                                    ),
                                }
                            }
                        }
                        Err(_) => {
                            warn!(
                                instance_id = instance.id.as_str(),
                                task_id = task_id.as_str(),
                                "workflow task not found during recovery"
                            );
                        }
                    }
                    continue;
                }

                if let Some(loop_state) = instance
                    .loop_state
                    .as_deref()
                    .and_then(|state| serde_json::from_str::<LoopState>(state).ok())
                    .filter(|state| state.parent_execution_id == exec.id)
                {
                    if let Some(active_execution_id) = loop_state.active_execution_id.as_deref() {
                        match self.instances.get_step_execution(active_execution_id) {
                            Ok(body_execution)
                                if matches!(
                                    body_execution.status.as_str(),
                                    "completed" | "failed"
                                ) =>
                            {
                                if let Some(task_id) = body_execution.task_id.as_deref() {
                                    let output = body_execution
                                        .output
                                        .clone()
                                        .unwrap_or_else(|| self.completed_task_output(task_id));
                                    let task_status = if body_execution.status == "completed" {
                                        "completed"
                                    } else {
                                        "failed"
                                    };
                                    if let Err(error) =
                                        self.on_task_completed(task_id, task_status, &output)
                                    {
                                        error!(
                                            instance_id = instance.id.as_str(),
                                            execution_id = body_execution.id.as_str(),
                                            error = %error,
                                            "failed to recover completed workflow loop task"
                                        );
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(error) => warn!(
                                instance_id = instance.id.as_str(),
                                execution_id = active_execution_id,
                                error = %error,
                                "workflow loop body execution is missing during recovery"
                            ),
                        }
                        continue;
                    }

                    let definition = match self.definition_for_instance(instance) {
                        Ok(definition) => definition,
                        Err(error) => {
                            error!(instance_id = instance.id.as_str(), error = %error, "failed to load workflow definition during loop recovery");
                            continue;
                        }
                    };
                    let trigger_data: Value = instance
                        .trigger_data
                        .as_deref()
                        .and_then(|value| serde_json::from_str(value).ok())
                        .unwrap_or(Value::Null);
                    let variables = self.load_variable_store(&instance.id)?;
                    if let Err(error) = self.continue_loop(
                        &instance.id,
                        loop_state,
                        &definition,
                        &trigger_data,
                        variables,
                    ) {
                        error!(instance_id = instance.id.as_str(), error = %error, "failed to resume workflow loop cursor");
                    }
                }
                continue;
            }

            // A wait can be durably completed one statement before its next
            // step is created. If the process stopped in that narrow window,
            // the persisted current position still identifies the wait and
            // advancing it is safe and idempotent.
            let definition = match self.definition_for_instance(instance) {
                Ok(definition) => definition,
                Err(error) => {
                    error!(instance_id = instance.id.as_str(), error = %error, "failed to load workflow definition during recovery");
                    continue;
                }
            };
            let current_step =
                usize::try_from(instance.current_step_index)
                    .ok()
                    .and_then(|index| {
                        definition
                            .flows
                            .get(&instance.current_flow)?
                            .steps
                            .get(index)
                    });
            let Some(step) = current_step.filter(|step| step.step_type == "wait") else {
                continue;
            };
            let Some(execution) = execs.iter().rfind(|execution| {
                execution.flow_name == instance.current_flow
                    && execution.step_id == step.id
                    && execution.status == "completed"
            }) else {
                continue;
            };
            let trigger_data: Value = instance
                .trigger_data
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or(Value::Null);
            let variables = self.load_variable_store(&instance.id)?;
            if let Err(error) =
                self.advance_completed_wait(execution, step, &definition, &trigger_data, &variables)
            {
                error!(
                    instance_id = instance.id.as_str(),
                    execution_id = execution.id.as_str(),
                    error = %error,
                    "failed to advance completed workflow wait during recovery"
                );
            }
        }

        Ok(())
    }

    /// Process unprocessed connector events, starting workflow instances
    /// for matching triggers.
    ///
    /// Returns the number of events processed.
    pub fn process_events(&self) -> Result<u32> {
        let events: Vec<(i64, String, String, String, String)> = self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT id, connector_id, channel_id, event_type, payload
                     FROM connector_events
                     WHERE processed = 0
                     ORDER BY created_at ASC
                     LIMIT 100",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok::<_, Error>(rows)
        })?;

        if events.is_empty() {
            return Ok(0);
        }

        let workflows = self.manager.list()?;
        let enabled_workflows: Vec<_> = workflows.iter().filter(|w| w.enabled).collect();

        let mut count = 0u32;

        for (event_id, connector_id, channel_id, event_type, payload_str) in &events {
            let payload: Value = serde_json::from_str(payload_str).unwrap_or(Value::Null);

            for wf in &enabled_workflows {
                let def = match WorkflowDefinition::parse(&wf.yaml_content) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                if let Some(ref trigger) = def.trigger {
                    if matches_trigger(trigger, connector_id, channel_id, event_type, &payload) {
                        info!(
                            workflow_id = wf.id.as_str(),
                            event_id,
                            connector = connector_id.as_str(),
                            event_type = event_type.as_str(),
                            "trigger matched, starting workflow instance"
                        );

                        match self.start_instance(&wf.id, payload.clone()) {
                            Ok(instance_id) => {
                                info!(
                                    workflow_id = wf.id.as_str(),
                                    instance_id = instance_id.as_str(),
                                    "workflow instance started from event"
                                );
                            }
                            Err(e) => {
                                error!(
                                    workflow_id = wf.id.as_str(),
                                    error = %e,
                                    "failed to start workflow instance from event"
                                );
                            }
                        }
                    }
                }
            }

            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE connector_events SET processed = 1 WHERE id = ?1",
                    [event_id],
                )
                .map_err(|e| Error::Database(e.to_string()))
            })?;

            count += 1;
        }

        Ok(count)
    }

    // -- Helper methods --

    fn load_variable_store(&self, instance_id: &str) -> Result<HashMap<String, Value>> {
        let json_str = self.instances.get_variable_store(instance_id)?;
        serde_json::from_str(&json_str)
            .map_err(|e| Error::Workflow(format!("failed to parse variable store: {e}")))
    }

    fn save_variable_store(&self, instance_id: &str, store: &HashMap<String, Value>) -> Result<()> {
        let json_str = serde_json::to_string(store)
            .map_err(|e| Error::Workflow(format!("failed to serialize variable store: {e}")))?;
        self.instances.update_variable_store(instance_id, &json_str)
    }
}

fn render_json_templates(value: &Value, ctx: &Value) -> Value {
    match value {
        Value::String(value) => Value::String(context::render_template(value, ctx)),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| render_json_templates(value, ctx))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), render_json_templates(value, ctx)))
                .collect(),
        ),
        value => value.clone(),
    }
}

fn parse_database_timestamp(value: &str) -> Option<chrono::DateTime<Utc>> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|timestamp| Utc.from_utc_datetime(&timestamp))
}

/// Native harnesses commonly wrap requested JSON in a Markdown fence or a
/// one-line explanation. Accept those conventional responses before falling
/// back to plain text so condition steps can reliably consume declared
/// outputs.
fn parse_task_output(output: &str) -> Value {
    let trimmed = output.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }

    for block in trimmed.split("```").skip(1).step_by(2) {
        let candidate = block
            .strip_prefix("json")
            .or_else(|| block.strip_prefix("JSON"))
            .unwrap_or(block)
            .trim();
        if let Ok(value) = serde_json::from_str::<Value>(candidate) {
            return value;
        }
    }

    for (open, close) in [('{', '}'), ('[', ']')] {
        let (Some(start), Some(end)) = (trimmed.find(open), trimmed.rfind(close)) else {
            continue;
        };
        if start >= end {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&trimmed[start..=end]) {
            return value;
        }
    }

    Value::String(output.to_string())
}

/// Check if a workflow trigger matches an incoming connector event.
pub fn matches_trigger(
    trigger: &WorkflowTrigger,
    event_connector: &str,
    event_channel: &str,
    event_type: &str,
    event_payload: &Value,
) -> bool {
    if trigger.connector != event_connector {
        return false;
    }
    if trigger.channel != event_channel {
        return false;
    }
    if trigger.event != event_type {
        return false;
    }
    for (key, expected) in &trigger.filter {
        let actual = event_payload.get(key);
        match actual {
            Some(val) => {
                if val != expected {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_conventional_structured_agent_outputs() {
        assert_eq!(
            parse_task_output("```json\n{\"verdict\":\"approved\"}\n```"),
            json!({"verdict": "approved"})
        );
        assert_eq!(
            parse_task_output("Done. {\"status\":\"complete\"}"),
            json!({"status": "complete"})
        );
        assert_eq!(
            parse_task_output("ordinary prose"),
            Value::String("ordinary prose".into())
        );
    }

    fn setup() -> (Arc<Database>, WorkflowEngine) {
        let db = Arc::new(Database::open_memory().unwrap());
        let engine = WorkflowEngine::new(db.clone());
        (db, engine)
    }

    const SIMPLE_WORKFLOW: &str = r#"
name: simple-pipeline
version: 1
flows:
  main:
    steps:
      - id: step1
        label: "First Step"
        agent: atlas
        prompt: "Do step 1: {{trigger.payload.summary}}"
      - id: step2
        label: "Second Step"
        agent: atlas
        prompt: "Do step 2 based on: @step1.output"
"#;

    fn create_workflow(db: &Arc<Database>, yaml: &str) -> String {
        let mgr = WorkflowManager::new(db.clone());
        let def = WorkflowDefinition::parse(yaml).unwrap();
        let record = mgr
            .create(&super::super::manager::CreateWorkflow {
                name: def.name.clone(),
                description: def.description.clone(),
                yaml_content: yaml.to_string(),
            })
            .unwrap();
        record.id
    }

    #[test]
    fn test_start_instance() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({"summary": "Test"}))
            .unwrap();

        // Instance should be running
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "running");
        assert_eq!(instance.current_flow, "main");
        assert_eq!(instance.current_step_index, 0);
        assert_eq!(instance.definition_yaml.as_deref(), Some(SIMPLE_WORKFLOW));

        // Step execution should exist with a task
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].step_id, "step1");
        assert_eq!(execs[0].flow_name, "main");
        assert_eq!(execs[0].status, "running");
        assert!(execs[0].task_id.is_some());
    }

    #[test]
    fn running_instance_keeps_its_definition_snapshot() {
        let (db, engine) = setup();
        let workflow_id = create_workflow(&db, SIMPLE_WORKFLOW);
        let instance_id = engine
            .start_instance(&workflow_id, json!({"summary": "Test"}))
            .unwrap();
        let first_task = engine.instances.list_step_executions(&instance_id).unwrap()[0]
            .task_id
            .clone()
            .unwrap();

        WorkflowManager::new(db.clone())
            .update(
                &workflow_id,
                &SIMPLE_WORKFLOW.replace(
                    "Do step 2 based on: @step1.output",
                    "This edited definition must not affect an active run",
                ),
            )
            .unwrap();
        engine
            .on_task_completed(&first_task, "completed", "original output")
            .unwrap();

        let second = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.step_id == "step2")
            .unwrap();
        let task = TaskBoard::new(db)
            .get(second.task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            task.description.as_deref(),
            Some("Do step 2 based on: original output")
        );
    }

    #[test]
    fn reusable_agent_input_selects_task_context_at_run_time() {
        let (db, engine) = setup();
        AgentRegistry::new(db.clone())
            .ensure("project-a", "codex")
            .unwrap();
        let workflow_id = create_workflow(
            &db,
            r#"
name: reusable
inputs:
  goal:
    type: string
    required: true
  worker:
    type: agent
    required: true
    primary: true
flows:
  main:
    steps:
      - id: work
        agent: "@worker"
        prompt: "Handle @goal"
"#,
        );

        let instance_id = engine
            .start_instance(
                &workflow_id,
                json!({"goal": "ship it", "worker": "project-a"}),
            )
            .unwrap();
        let execution = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .pop()
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .get(execution.task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(task.agent_id.as_deref(), Some("project-a"));
        assert_eq!(task.description.as_deref(), Some("Handle ship it"));

        let error = engine
            .start_instance(
                &workflow_id,
                json!({"goal": "ship it", "worker": "missing"}),
            )
            .unwrap_err();
        assert!(error.to_string().contains("unknown agent 'missing'"));
    }

    #[test]
    fn conversation_workflows_adopt_and_enforce_the_project_boundary() {
        let (db, engine) = setup();
        let registry = AgentRegistry::new(db.clone());
        registry.ensure("project-a", "codex").unwrap();
        registry.ensure("project-b", "codex").unwrap();
        let conversation = crate::conversations::ConversationManager::new(db.clone())
            .create_in_project(
                Some("project-a"),
                &crate::conversations::CreateConversation {
                    title: Some("Launch".into()),
                    icon: None,
                    participant_ids: vec!["project-a".into()],
                },
            )
            .unwrap();
        let workflow_id = create_workflow(
            &db,
            r#"
name: project-bound
inputs:
  worker: { type: agent, required: true, primary: true }
flows:
  main:
    steps:
      - id: work
        agent: "@worker"
        prompt: "Handle the conversation work"
"#,
        );

        let instance_id = engine
            .start_instance_in_context(
                &workflow_id,
                json!({"worker": "project-a"}),
                WorkflowContext {
                    project_id: None,
                    conversation_id: Some(conversation.id.clone()),
                },
            )
            .unwrap();
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.project_id.as_deref(), Some("project-a"));
        assert_eq!(
            instance.conversation_id.as_deref(),
            Some(conversation.id.as_str())
        );
        let execution = engine.instances.list_step_executions(&instance_id).unwrap()[0].clone();
        let task = TaskBoard::new(db.clone())
            .get(execution.task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(task.project_id.as_deref(), Some("project-a"));
        assert_eq!(
            task.conversation_id.as_deref(),
            Some(conversation.id.as_str())
        );

        let error = engine
            .start_instance_in_context(
                &workflow_id,
                json!({"worker": "project-b"}),
                WorkflowContext {
                    project_id: Some("project-a".into()),
                    conversation_id: Some(conversation.id),
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("outside project 'project-a'"));
        assert_eq!(
            engine
                .instances
                .list_instances(&workflow_id, 10)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn agent_started_conversation_workflows_require_current_membership() {
        let (db, engine) = setup();
        AgentRegistry::new(db.clone())
            .ensure("project-a", "codex")
            .unwrap();
        let conversations = crate::conversations::ConversationManager::new(db.clone());
        let conversation = conversations
            .create_in_project(
                Some("project-a"),
                &crate::conversations::CreateConversation {
                    title: Some("Launch".into()),
                    icon: None,
                    participant_ids: vec!["project-a".into()],
                },
            )
            .unwrap();
        let workflow_id = create_workflow(
            &db,
            r#"
name: agent-started
inputs:
  worker: { type: agent, required: true, primary: true }
flows:
  main:
    steps:
      - id: work
        agent: "@worker"
        prompt: "Handle the conversation work"
"#,
        );
        let context = || WorkflowContext {
            project_id: Some("project-a".into()),
            conversation_id: Some(conversation.id.clone()),
        };

        engine
            .start_instance_in_context_for_conversation_agent(
                &workflow_id,
                json!({"worker": "project-a"}),
                context(),
                "project-a",
            )
            .unwrap();
        conversations
            .remove_participant(&conversation.id, "agent", "project-a")
            .unwrap();
        let before = engine
            .instances
            .list_instances(&workflow_id, 10)
            .unwrap()
            .len();

        let rejected = engine.start_instance_in_context_for_conversation_agent(
            &workflow_id,
            json!({"worker": "project-a"}),
            context(),
            "project-a",
        );
        assert!(matches!(rejected, Err(Error::Conversation(_))));
        assert_eq!(
            engine
                .instances
                .list_instances(&workflow_id, 10)
                .unwrap()
                .len(),
            before
        );
    }

    #[test]
    fn conversation_workflow_creation_rolls_back_if_publication_fails() {
        let (db, engine) = setup();
        AgentRegistry::new(db.clone())
            .ensure("project-a", "codex")
            .unwrap();
        let conversation = crate::conversations::ConversationManager::new(db.clone())
            .create_in_project(
                Some("project-a"),
                &crate::conversations::CreateConversation {
                    title: Some("Atomic workflow".into()),
                    icon: None,
                    participant_ids: vec!["project-a".into()],
                },
            )
            .unwrap();
        let workflow_id = create_workflow(
            &db,
            r#"
name: atomic-conversation-workflow
inputs:
  worker: { type: agent, required: true, primary: true }
flows:
  main:
    steps:
      - id: work
        agent: "@worker"
        prompt: "Handle the conversation work"
"#,
        );
        let context = || WorkflowContext {
            project_id: Some("project-a".into()),
            conversation_id: Some(conversation.id.clone()),
        };
        let message = SendMessage {
            sender_type: "user".into(),
            sender_id: "local".into(),
            sender_name: Some("You".into()),
            content: "Started workflow: Atomic workflow".into(),
            message_type: Some("workflow".into()),
        };
        db.with_conn(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_workflow_publication
                 BEFORE INSERT ON conversation_messages
                 WHEN NEW.message_type = 'workflow'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced workflow publication failure');
                 END;",
            )
        })
        .unwrap();

        let failed = engine.start_instance_in_context_with_conversation_message(
            &workflow_id,
            json!({"worker": "project-a"}),
            context(),
            None,
            &message,
        );
        assert!(failed.is_err());
        let rolled_back = db
            .with_conn(|conn| {
                Ok::<_, Error>((
                    conn.query_row("SELECT COUNT(*) FROM workflow_instances", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get::<_, i64>(0))?,
                    conn.query_row("SELECT COUNT(*) FROM task_queue", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM conversation_messages", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                ))
            })
            .unwrap();
        assert_eq!(rolled_back, (0, 0, 0, 0));

        db.with_conn(|conn| conn.execute_batch("DROP TRIGGER fail_workflow_publication"))
            .unwrap();
        let (instance_id, published) = engine
            .start_instance_in_context_with_conversation_message(
                &workflow_id,
                json!({"worker": "project-a"}),
                context(),
                None,
                &message,
            )
            .unwrap();
        assert_eq!(published.metadata["workflow_id"], workflow_id);
        assert_eq!(published.metadata["instance_id"], instance_id);
        assert_eq!(
            engine
                .instances
                .list_step_executions(&instance_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn workflow_steps_render_native_commands_and_session_controls() {
        let (db, engine) = setup();
        let workflow = r#"
name: native-controls
version: 1
flows:
  main:
    steps:
      - id: optimize
        agent: atlas
        command: /loop
        prompt: "Improve {{trigger.payload.page}}"
        new_session: true
        session_config:
          mode: build
          thought_level: "{{trigger.payload.effort}}"
          use_fast_tools: true
        mcp_server: seo
        mcp_tool: audit_page
        mcp_arguments:
          page: "{{trigger.payload.page}}"
          depth: 2
"#;
        let workflow_id = create_workflow(&db, workflow);
        let instance_id = engine
            .start_instance(
                &workflow_id,
                json!({ "page": "the pricing page", "effort": "high" }),
            )
            .unwrap();
        let execution = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let task = TaskBoard::new(db)
            .get(execution.task_id.as_deref().unwrap())
            .unwrap();

        let description = task.description.as_deref().unwrap();
        assert!(description
            .starts_with("/loop Call the MCP tool 'audit_page' from the attached 'seo' server"));
        assert!(description.contains("\"page\": \"the pricing page\""));
        assert!(description.ends_with("Improve the pricing page"));
        assert_eq!(task.context.as_ref().unwrap()["session_mode"], "new");
        assert_eq!(
            task.context.as_ref().unwrap()["session_config"],
            json!({
                "mode": "build",
                "thought_level": "high",
                "use_fast_tools": true
            })
        );
    }

    #[test]
    fn test_on_task_completed_advances() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({"summary": "Test"}))
            .unwrap();

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        engine
            .on_task_completed(&task_id, "completed", "Step 1 output")
            .unwrap();

        // Should have advanced to step2
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_flow, "main");
        assert_eq!(instance.current_step_index, 1);

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 2);
        assert_eq!(execs[1].step_id, "step2");
    }

    #[test]
    fn test_terminal_step_completes_workflow() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({"summary": "Test"}))
            .unwrap();

        // Complete step1
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task1_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task1_id, "completed", "output1")
            .unwrap();

        // Complete step2 (last step)
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task2_id = execs[1].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task2_id, "completed", "output2")
            .unwrap();

        // Workflow should be completed
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "completed");
    }

    #[test]
    fn test_on_task_completed_not_in_workflow() {
        let (_db, engine) = setup();
        engine
            .on_task_completed("nonexistent-task", "completed", "output")
            .unwrap();
    }

    #[test]
    fn test_sink_step() {
        let yaml = r#"
name: sink-test
version: 1
flows:
  main:
    steps:
      - id: work
        label: "Do Work"
        agent: atlas
        prompt: "Work"
      - id: notify
        type: sink
        sinks:
          - connector: telegram
            channel: dev-chat
            template: "Done: @work.output"
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Complete the work step
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task_id, "completed", "work output")
            .unwrap();

        // Sink step should be completed and workflow should be done
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "completed");

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 2);
        assert_eq!(execs[1].step_id, "notify");
        assert_eq!(execs[1].status, "completed");
    }

    #[test]
    fn test_when_step_branching() {
        let yaml = r#"
name: when-test
version: 1
flows:
  main:
    steps:
      - id: classify
        agent: atlas
        prompt: "Classify"
        outputs:
          intent:
            type: string
      - id: route
        type: when
        switch: "@classify.intent"
        arms:
          - match: bug
            goto: "flow bug_flow"
          - match: default
            continue: true
      - id: default_reply
        type: sink
        sinks:
          - connector: telegram
            channel: test
            template: "Default reply"
  bug_flow:
    steps:
      - id: investigate
        agent: atlas
        prompt: "Investigate bug"
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        // Test: classify returns "bug" -> should go to bug_flow
        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        engine
            .on_task_completed(&task_id, "completed", r#"{"intent": "bug"}"#)
            .unwrap();

        // Should have jumped to bug_flow
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_flow, "bug_flow");
        assert_eq!(instance.current_step_index, 0);
    }

    #[test]
    fn test_when_step_default_continue() {
        let yaml = r#"
name: when-default
version: 1
flows:
  main:
    steps:
      - id: classify
        agent: atlas
        prompt: "Classify"
        outputs:
          intent:
            type: string
      - id: route
        type: when
        switch: "@classify.intent"
        arms:
          - match: bug
            goto: "flow bug_flow"
          - match: default
            continue: true
      - id: default_reply
        type: sink
        sinks:
          - connector: telegram
            channel: test
            template: "Default"
  bug_flow:
    steps:
      - id: investigate
        agent: atlas
        prompt: "Investigate"
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        // Return "question" — should hit default and continue to next step
        engine
            .on_task_completed(&task_id, "completed", r#"{"intent": "question"}"#)
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        // Should have completed (default_reply is a sink, which auto-completes)
        assert_eq!(instance.status, "completed");
    }

    #[test]
    fn test_jump_step() {
        let yaml = r#"
name: jump-test
version: 1
flows:
  main:
    steps:
      - id: start
        agent: atlas
        prompt: "Start"
      - id: go
        type: jump
        target: "flow other"
  other:
    steps:
      - id: finish
        agent: atlas
        prompt: "Finish"
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Complete start step
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task_id, "completed", "started")
            .unwrap();

        // Should have jumped to other flow
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_flow, "other");
        assert_eq!(instance.current_step_index, 0);
    }

    #[test]
    fn test_variable_store_populated() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({"summary": "Test"}))
            .unwrap();

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        engine
            .on_task_completed(&task_id, "completed", r#"{"result": "done"}"#)
            .unwrap();

        // Variable store should have step1's output
        let store = engine.load_variable_store(&instance_id).unwrap();
        assert!(store.contains_key("step1"));
    }

    #[test]
    fn test_failed_step_with_error_flow() {
        let yaml = r#"
name: error-handling
version: 1
flows:
  main:
    steps:
      - id: risky
        agent: atlas
        prompt: "Do risky thing"
  on_error:
    steps:
      - id: handle_error
        agent: atlas
        prompt: "Handle the error"
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        engine
            .on_task_completed(&task_id, "failed", "something broke")
            .unwrap();

        // Should have jumped to on_error flow
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_flow, "on_error");
        assert_eq!(instance.status, "running");
    }

    #[test]
    fn test_failed_step_without_error_flow() {
        let yaml = r#"
name: no-error-handler
version: 1
flows:
  main:
    steps:
      - id: risky
        agent: atlas
        prompt: "Do risky thing"
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        engine
            .on_task_completed(&task_id, "failed", "something broke")
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "failed");
    }

    #[test]
    fn test_matches_trigger_basic() {
        let trigger = WorkflowTrigger {
            connector: "jira".into(),
            channel: "my-project".into(),
            event: "issue_created".into(),
            filter: std::collections::HashMap::new(),
        };

        assert!(matches_trigger(
            &trigger,
            "jira",
            "my-project",
            "issue_created",
            &serde_json::json!({})
        ));

        assert!(!matches_trigger(
            &trigger,
            "github",
            "my-project",
            "issue_created",
            &serde_json::json!({})
        ));

        assert!(!matches_trigger(
            &trigger,
            "jira",
            "other-project",
            "issue_created",
            &serde_json::json!({})
        ));

        assert!(!matches_trigger(
            &trigger,
            "jira",
            "my-project",
            "issue_updated",
            &serde_json::json!({})
        ));
    }

    #[test]
    fn test_matches_trigger_with_filter() {
        let mut filter = std::collections::HashMap::new();
        filter.insert("type".to_string(), serde_json::json!("Story"));

        let trigger = WorkflowTrigger {
            connector: "jira".into(),
            channel: "my-project".into(),
            event: "issue_created".into(),
            filter,
        };

        assert!(matches_trigger(
            &trigger,
            "jira",
            "my-project",
            "issue_created",
            &serde_json::json!({"type": "Story", "priority": "High"})
        ));

        assert!(!matches_trigger(
            &trigger,
            "jira",
            "my-project",
            "issue_created",
            &serde_json::json!({"type": "Bug"})
        ));
    }

    #[test]
    fn test_process_events() {
        let (db, engine) = setup();

        let yaml = r#"
name: event-workflow
version: 1
trigger:
  connector: webhook
  channel: incoming
  event: message
flows:
  main:
    steps:
      - id: handle
        agent: atlas
        prompt: "Handle: {{trigger.payload.text}}"
"#;
        let _wf_id = create_workflow(&db, yaml);

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO connector_events (connector_id, channel_id, event_type, payload, processed)
                 VALUES ('webhook', 'incoming', 'message', '{\"text\": \"hello\"}', 0)",
                [],
            )
            .unwrap();
        });

        let count = engine.process_events().unwrap();
        assert_eq!(count, 1);

        let processed: i32 = db.with_conn(|conn| {
            conn.query_row(
                "SELECT processed FROM connector_events LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
        });
        assert_eq!(processed, 1);
    }

    #[test]
    fn test_recover_no_running() {
        let (_db, engine) = setup();
        engine.recover().unwrap();
    }

    #[test]
    fn test_find_execution_by_task() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap();

        let found = engine.find_execution_by_task(task_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().step_id, "step1");

        let not_found = engine.find_execution_by_task("no-such-task").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_output_schema_in_prompt() {
        let yaml = r#"
name: output-schema-test
version: 1
flows:
  main:
    steps:
      - id: classify
        agent: atlas
        prompt: "Classify this"
        outputs:
          intent:
            type: string
            description: "The intent category"
          confidence:
            type: number
            description: "Confidence score 0-1"
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Verify the task was created (the prompt would contain schema info)
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 1);
        assert!(execs[0].task_id.is_some());
    }

    #[test]
    fn test_recover_running_instance_with_completed_task() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        // Start a workflow — creates task for step1
        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({"summary": "test"}))
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "running");
        assert_eq!(instance.current_flow, "main");
        assert_eq!(instance.current_step_index, 0);

        // Get the task that was created
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        // Simulate: task completed during downtime (update task status directly)
        let board = TaskBoard::new(db.clone());
        board.update_status(&task_id, "completed", None).unwrap();

        // Simulate server restart — create a new engine instance and call recover
        let engine2 = WorkflowEngine::new(db.clone());
        engine2.recover().unwrap();

        // The instance should have advanced past step1
        let instance = engine2.instances.get_instance(&instance_id).unwrap();
        // It should now be at step2 (index 1), or still running with a new task
        assert_eq!(instance.status, "running");
        // Step execution for step1 should be completed
        let execs = engine2
            .instances
            .list_step_executions(&instance_id)
            .unwrap();
        let step1_exec = execs.iter().find(|e| e.step_id == "step1").unwrap();
        assert_eq!(step1_exec.status, "completed");
        // Step2 should have been started
        assert!(execs.iter().any(|e| e.step_id == "step2"));
    }

    #[test]
    fn test_recover_running_instance_task_still_pending() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Task is still pending (not completed) — recovery should leave it alone
        let engine2 = WorkflowEngine::new(db.clone());
        engine2.recover().unwrap();

        let instance = engine2.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "running");
        assert_eq!(instance.current_step_index, 0);
        // Only one step execution — step1 still running
        let execs = engine2
            .instances
            .list_step_executions(&instance_id)
            .unwrap();
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].step_id, "step1");
        assert_eq!(execs[0].status, "running");
    }

    #[test]
    fn test_full_pipeline_two_steps() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({"summary": "build feature"}))
            .unwrap();

        // Step1 task created
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 1);
        let task1_id = execs[0].task_id.as_ref().unwrap().clone();

        // Complete step1 with output
        engine
            .on_task_completed(&task1_id, "completed", r#"{"output": "step1 done"}"#)
            .unwrap();

        // Step2 should now be running
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "running");
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 2);
        let step2_exec = execs.iter().find(|e| e.step_id == "step2").unwrap();
        assert_eq!(step2_exec.status, "running");
        let task2_id = step2_exec.task_id.as_ref().unwrap().clone();

        // Complete step2 — workflow should finish
        engine
            .on_task_completed(&task2_id, "completed", "all done")
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "completed");
    }

    #[test]
    fn test_when_cycle_back_to_earlier_step() {
        let (db, engine) = setup();

        let yaml = r#"
name: cycle-test
version: 1
flows:
  main:
    steps:
      - id: do_work
        type: step
        label: "Do Work"
        agent: dev
        prompt: "Do work"
      - id: review
        type: step
        label: "Review"
        agent: reviewer
        prompt: "Review work"
      - id: check
        type: when
        label: "Check verdict"
        switch: "@review.verdict"
        arms:
          - match: "approved"
            continue: true
          - match: "rejected"
            goto: step do_work
      - id: done
        type: sink
        label: "Notify"
        sinks:
          - connector: webhook
            channel: alerts
            template: "Done"
"#;
        let wf_id = create_workflow(&db, yaml);
        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Complete do_work
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task1_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task1_id, "completed", r#"{"output": "first attempt"}"#)
            .unwrap();

        // Complete review with rejected verdict
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let review_exec = execs
            .iter()
            .find(|e| e.step_id == "review" && e.status == "running")
            .unwrap();
        let task2_id = review_exec.task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task2_id, "completed", r#"{"verdict": "rejected"}"#)
            .unwrap();

        // Should cycle back to do_work (step 0)
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "running");
        assert_eq!(instance.current_step_index, 0);

        // do_work should have a second execution
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let do_work_execs: Vec<_> = execs.iter().filter(|e| e.step_id == "do_work").collect();
        assert!(do_work_execs.len() >= 2, "do_work should run a second time");
    }

    #[test]
    fn durable_wait_resumes_with_event_payload_and_survives_recovery() {
        let (db, engine) = setup();
        AgentRegistry::new(db.clone())
            .ensure("project-a", "codex")
            .unwrap();
        let workflow_id = create_workflow(
            &db,
            r#"
name: pr-review
inputs:
  worker:
    type: agent
    required: true
flows:
  main:
    steps:
      - id: publish
        agent: "@worker"
        prompt: publish
        outputs:
          pull_request_url: { type: string }
      - id: wait_for_review
        type: wait
        agent: "@worker"
        event: github.pull_request.activity
        resource: "@publish.pull_request_url"
        timeout: 14d
      - id: respond
        agent: "@worker"
        prompt: "Respond to @wait_for_review.body from @wait_for_review.author"
"#,
        );
        let instance_id = engine
            .start_instance(&workflow_id, json!({"worker": "project-a"}))
            .unwrap();
        let publish_task = engine.instances.list_step_executions(&instance_id).unwrap()[0]
            .task_id
            .clone()
            .unwrap();
        engine
            .on_task_completed(
                &publish_task,
                "completed",
                r#"{"pull_request_url":"https://github.com/XpressAI/xpressclaw/pull/143"}"#,
            )
            .unwrap();
        let wait = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.step_id == "wait_for_review")
            .unwrap();
        assert_eq!(wait.status, "waiting");
        let state: WaitState =
            serde_json::from_str(wait.input_context.as_deref().unwrap()).unwrap();
        assert_eq!(state.agent_id, "project-a");
        assert_eq!(
            state.resource,
            "https://github.com/XpressAI/xpressclaw/pull/143"
        );

        assert!(engine
            .instances
            .claim_wait(
                &wait.id,
                &json!({"kind":"review_comment", "body":"Add a regression test", "author":"reviewer"}).to_string(),
            )
            .unwrap());
        WorkflowEngine::new(db.clone()).recover().unwrap();

        let executions = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(
            executions
                .iter()
                .find(|execution| execution.id == wait.id)
                .unwrap()
                .status,
            "completed"
        );
        let response = executions
            .iter()
            .find(|execution| execution.step_id == "respond")
            .unwrap();
        let task = TaskBoard::new(db)
            .get(response.task_id.as_deref().unwrap())
            .unwrap();
        assert!(task
            .description
            .as_deref()
            .unwrap()
            .contains("Add a regression test from reviewer"));
    }

    #[test]
    fn reusable_code_review_runs_the_full_review_lifecycle() {
        let (db, engine) = setup();
        let registry = AgentRegistry::new(db.clone());
        registry.ensure("implementation-context", "codex").unwrap();
        registry.ensure("review-context", "opencode").unwrap();
        let workflow_id = create_workflow(
            &db,
            r#"
name: reusable-code-review
inputs:
  goal: { type: string, required: true }
  implementer: { type: agent, required: true, primary: true }
  reviewer: { type: agent, required: true }
  wait_for_human_review: { type: boolean, default: true }
flows:
  main:
    steps:
      - id: implement
        agent: "@implementer"
        prompt: "Implement @goal on a draft PR"
        outputs:
          pull_request_url: { type: string }
      - id: review
        agent: "@reviewer"
        new_session: true
        prompt: "Review @implement.pull_request_url"
        outputs:
          verdict: { type: string }
          feedback: { type: string }
      - id: review_result
        type: when
        switch: "@review.verdict"
        arms:
          - { match: approved, goto: "step mark_ready" }
          - { match: changes_requested, continue: true }
          - { match: default, goto: "step review" }
      - id: revise
        agent: "@implementer"
        prompt: "Address @review.feedback"
      - id: repeat_review
        type: jump
        target: step review
      - id: mark_ready
        agent: "@implementer"
        prompt: "Mark @implement.pull_request_url ready"
        outputs:
          pull_request_url: { type: string }
      - id: human_review_gate
        type: when
        switch: "@wait_for_human_review"
        arms:
          - { match: "true", continue: true }
          - { match: "false", goto: "flow done" }
      - id: wait_for_review
        type: wait
        agent: "@implementer"
        event: github.pull_request.activity
        resource: "@mark_ready.pull_request_url"
        timeout: 14d
        on_timeout: flow timed_out
      - id: handle_review
        agent: "@implementer"
        prompt: "Handle @wait_for_review.body"
        outputs:
          outcome: { type: string }
      - id: human_review_result
        type: when
        switch: "@handle_review.outcome"
        arms:
          - { match: approved, goto: "flow done" }
          - { match: changes_addressed, goto: "step review" }
          - { match: keep_waiting, goto: "step wait_for_review" }
          - { match: default, goto: "step wait_for_review" }
  done: { steps: [] }
  timed_out: { steps: [] }
"#,
        );
        let instance_id = engine
            .start_instance(
                &workflow_id,
                json!({
                    "goal": "Ship the feature",
                    "implementer": "implementation-context",
                    "reviewer": "review-context"
                }),
            )
            .unwrap();

        let running_task = |step_id: &str| {
            engine
                .instances
                .list_step_executions(&instance_id)
                .unwrap()
                .into_iter()
                .rev()
                .find(|execution| execution.step_id == step_id && execution.status == "running")
                .and_then(|execution| execution.task_id)
                .unwrap()
        };

        engine
            .on_task_completed(
                &running_task("implement"),
                "completed",
                r#"{"pull_request_url":"https://github.com/XpressAI/xpressclaw/pull/200"}"#,
            )
            .unwrap();
        let first_review = running_task("review");
        let first_review_task = TaskBoard::new(db.clone()).get(&first_review).unwrap();
        assert_eq!(
            first_review_task.agent_id.as_deref(),
            Some("review-context")
        );
        assert_eq!(
            first_review_task.context.as_ref().unwrap()["session_mode"],
            "new"
        );
        assert!(first_review_task
            .description
            .as_deref()
            .unwrap()
            .contains("https://github.com/XpressAI/xpressclaw/pull/200"));

        engine
            .on_task_completed(
                &first_review,
                "completed",
                "Review complete. ```json\n{\"verdict\":\"changes_requested\",\"feedback\":\"Add coverage\"}\n```",
            )
            .unwrap();
        let revise = running_task("revise");
        let revise_task = TaskBoard::new(db.clone()).get(&revise).unwrap();
        assert_eq!(
            revise_task.agent_id.as_deref(),
            Some("implementation-context")
        );
        assert!(revise_task
            .description
            .as_deref()
            .unwrap()
            .contains("Add coverage"));
        let board = TaskBoard::new(db.clone());
        board
            .sync_reported_subtasks(
                &revise,
                "revise-attempt",
                &[
                    crate::tasks::board::ReportedSubtask {
                        title: "Address independent review feedback".to_string(),
                        status: crate::tasks::board::TaskStatus::Completed,
                    },
                    crate::tasks::board::ReportedSubtask {
                        title: "Address any further review feedback through approval or merge"
                            .to_string(),
                        status: crate::tasks::board::TaskStatus::InProgress,
                    },
                ],
            )
            .unwrap();
        board
            .defer_reported_subtasks(&revise, "successful_attempt_completed")
            .unwrap();
        assert!(board.subtasks_complete(&revise).unwrap());
        engine
            .on_task_completed(&revise, "completed", "Coverage added")
            .unwrap();

        let repeated_review = running_task("review");
        assert_ne!(repeated_review, first_review);
        let future_plan = board
            .list_subtasks(&revise)
            .unwrap()
            .into_iter()
            .find(|task| task.title.contains("approval or merge"))
            .unwrap();
        assert_eq!(
            future_plan.status,
            crate::tasks::board::TaskStatus::Cancelled
        );
        assert!(!future_plan.blocks_parent);

        engine
            .on_task_completed(
                &repeated_review,
                "completed",
                r#"{"verdict":"approved","feedback":"Looks good"}"#,
            )
            .unwrap();
        let mark_ready = running_task("mark_ready");
        engine
            .on_task_completed(
                &mark_ready,
                "completed",
                r#"{"pull_request_url":"https://github.com/XpressAI/xpressclaw/pull/200"}"#,
            )
            .unwrap();

        let first_wait = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .rev()
            .find(|execution| {
                execution.step_id == "wait_for_review" && execution.status == "waiting"
            })
            .unwrap();
        let first_wait_state: WaitState =
            serde_json::from_str(first_wait.input_context.as_deref().unwrap()).unwrap();
        let mark_ready_started_at = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.step_id == "mark_ready")
            .and_then(|execution| execution.started_at)
            .and_then(|timestamp| parse_database_timestamp(&timestamp))
            .unwrap();
        assert_eq!(
            first_wait_state.started_at,
            mark_ready_started_at.to_rfc3339()
        );
        let event = json!({
            "kind": "review_comment",
            "id": 42,
            "body": "Please rename this",
            "author": "human-reviewer",
            "created_at": "2026-08-02T12:00:01Z",
            "cursor": "review_comment:00000000000000000042"
        });
        engine.resume_wait_execution(&first_wait.id, event).unwrap();
        engine
            .on_task_completed(
                &running_task("handle_review"),
                "completed",
                r#"{"outcome":"keep_waiting"}"#,
            )
            .unwrap();

        let second_wait = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .rev()
            .find(|execution| {
                execution.step_id == "wait_for_review" && execution.status == "waiting"
            })
            .unwrap();
        let state: WaitState =
            serde_json::from_str(second_wait.input_context.as_deref().unwrap()).unwrap();
        assert_eq!(state.started_at, "2026-08-02T12:00:01+00:00");
        assert_eq!(
            state.after_cursor.as_deref(),
            Some("review_comment:00000000000000000042")
        );
    }

    #[test]
    fn recovery_advances_an_event_committed_before_the_next_step() {
        let (db, engine) = setup();
        let workflow_id = create_workflow(
            &db,
            r#"
name: recover-completed-wait
flows:
  main:
    steps:
      - id: wait_for_review
        type: wait
        agent: project-a
        event: github.pull_request.comment
        resource: https://github.com/XpressAI/xpressclaw/pull/143
      - id: respond
        agent: project-a
        prompt: "Respond to @wait_for_review.body"
"#,
        );
        let instance_id = engine.start_instance(&workflow_id, json!({})).unwrap();
        let wait = engine.instances.list_step_executions(&instance_id).unwrap()[0].clone();
        let payload = json!({"kind":"review_comment", "body":"Please add coverage"});
        let payload_json = payload.to_string();
        assert!(engine
            .instances
            .claim_wait(&wait.id, &payload_json)
            .unwrap());

        // Simulate a process stop after the event and completed execution were
        // committed but immediately before the downstream task was created.
        let mut variables = engine.load_variable_store(&instance_id).unwrap();
        variables.insert(wait.step_id.clone(), payload);
        engine
            .save_variable_store(&instance_id, &variables)
            .unwrap();
        engine
            .instances
            .update_step_status(&wait.id, "completed", Some(&payload_json))
            .unwrap();
        engine
            .instances
            .set_active_status(&instance_id, "running")
            .unwrap();

        WorkflowEngine::new(db.clone()).recover().unwrap();
        let response = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.step_id == "respond")
            .unwrap();
        let task = TaskBoard::new(db)
            .get(response.task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            task.description.as_deref(),
            Some("Respond to Please add coverage")
        );
    }

    #[test]
    fn wait_timeout_takes_configured_flow() {
        let (db, engine) = setup();
        let workflow_id = create_workflow(
            &db,
            r#"
name: pr-review-timeout
flows:
  main:
    steps:
      - id: wait_for_review
        type: wait
        agent: project-a
        event: github.pull_request.review
        resource: https://github.com/XpressAI/xpressclaw/pull/143
        timeout: 1s
        on_timeout: flow expired
  expired:
    steps: []
"#,
        );
        let instance_id = engine.start_instance(&workflow_id, json!({})).unwrap();
        let wait = engine.instances.list_step_executions(&instance_id).unwrap()[0].clone();
        engine.timeout_wait_execution(&wait.id).unwrap();
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "completed");
        assert_eq!(instance.current_flow, "expired");
    }

    #[test]
    fn test_loop_step_iterates() {
        let (db, engine) = setup();

        let yaml = r#"
name: loop-test
version: 1
flows:
  main:
    steps:
      - id: prep
        type: step
        label: "Prepare"
        agent: atlas
        prompt: "Prepare items"
        outputs:
          items: { type: array }
      - id: process
        type: loop
        label: "Process Items"
        over: "@prep.items"
        as: item
        steps:
          - id: handle
            type: step
            label: "Handle"
            agent: atlas
            prompt: "Handle: @item"
      - id: finish
        type: sink
        label: "Done"
        sinks: []
"#;
        let wf_id = create_workflow(&db, yaml);
        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Complete prep with an array of 2 items
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task_id, "completed", r#"{"items": ["a", "b"]}"#)
            .unwrap();

        let first_handle = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.step_id == "handle" && execution.status == "running")
            .unwrap();
        let first_task = TaskBoard::new(db.clone())
            .get(first_handle.task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(first_task.description.as_deref(), Some("Handle: a"));
        engine
            .on_task_completed(
                first_handle.task_id.as_deref().unwrap(),
                "completed",
                "a done",
            )
            .unwrap();

        let second_handle = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .rev()
            .find(|execution| execution.step_id == "handle" && execution.status == "running")
            .unwrap();
        let second_task = TaskBoard::new(db.clone())
            .get(second_handle.task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(second_task.description.as_deref(), Some("Handle: b"));
        engine
            .on_task_completed(
                second_handle.task_id.as_deref().unwrap(),
                "completed",
                "b done",
            )
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "completed");
        assert!(instance.loop_state.is_none());
        let executions = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(
            executions
                .iter()
                .filter(|execution| execution.step_id == "handle")
                .count(),
            2
        );
    }

    #[test]
    fn loop_recovery_advances_a_completed_body_cursor_once() {
        let (db, engine) = setup();
        let workflow_id = create_workflow(
            &db,
            r#"
name: recover-loop
flows:
  main:
    steps:
      - id: prep
        agent: atlas
        prompt: Prepare
        outputs:
          items: { type: array }
      - id: process
        type: loop
        over: "@prep.items"
        as: item
        steps:
          - id: handle
            agent: atlas
            prompt: "Handle: @item"
"#,
        );
        let instance_id = engine.start_instance(&workflow_id, json!({})).unwrap();
        let prep = engine.instances.list_step_executions(&instance_id).unwrap()[0]
            .task_id
            .clone()
            .unwrap();
        engine
            .on_task_completed(&prep, "completed", r#"{"items":["a","b"]}"#)
            .unwrap();
        let first = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.step_id == "handle")
            .unwrap();

        // Persist the body completion without moving the loop cursor, which
        // is the exact crash boundary recovery must replay.
        engine
            .instances
            .update_step_status(&first.id, "completed", Some("a done"))
            .unwrap();
        WorkflowEngine::new(db.clone()).recover().unwrap();

        let handles: Vec<_> = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .filter(|execution| execution.step_id == "handle")
            .collect();
        assert_eq!(handles.len(), 2);
        let second = handles.last().unwrap();
        let task = TaskBoard::new(db)
            .get(second.task_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(task.description.as_deref(), Some("Handle: b"));
    }

    #[test]
    fn loop_recovery_dispatches_a_persisted_body_owner_once() {
        let (db, engine) = setup();
        let workflow_id = create_workflow(
            &db,
            r#"
name: recover-loop-dispatch
flows:
  main:
    steps:
      - id: prep
        agent: atlas
        prompt: Prepare
        outputs:
          items: { type: array }
      - id: process
        type: loop
        over: "@prep.items"
        as: item
        steps:
          - id: handle
            agent: atlas
            prompt: "Handle: @item"
"#,
        );
        let instance_id = engine.start_instance(&workflow_id, json!({})).unwrap();
        let prep = engine.instances.list_step_executions(&instance_id).unwrap()[0]
            .task_id
            .clone()
            .unwrap();
        engine
            .on_task_completed(&prep, "completed", r#"{"items":["a"]}"#)
            .unwrap();
        let handle = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .find(|execution| execution.step_id == "handle")
            .unwrap();
        let task_id = handle.task_id.clone().unwrap();
        let loop_state: LoopState = serde_json::from_str(
            engine
                .instances
                .get_instance(&instance_id)
                .unwrap()
                .loop_state
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            loop_state.active_execution_id.as_deref(),
            Some(handle.id.as_str())
        );

        // Recreate the crash boundary after task ownership commits but before
        // dispatch. Recovery must enqueue that exact task without creating a
        // second body execution, even when recovery itself runs twice.
        db.with_conn(|conn| conn.execute("DELETE FROM task_queue WHERE task_id = ?1", [&task_id]))
            .unwrap();
        WorkflowEngine::new(db.clone()).recover().unwrap();
        WorkflowEngine::new(db.clone()).recover().unwrap();

        let handles = engine
            .instances
            .list_step_executions(&instance_id)
            .unwrap()
            .into_iter()
            .filter(|execution| execution.step_id == "handle")
            .collect::<Vec<_>>();
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].id, handle.id);
        let active_dispatches: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM task_queue
                     WHERE task_id = ?1 AND status IN ('queued', 'running')",
                    [&task_id],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(active_dispatches, 1);
    }

    #[test]
    fn test_cross_flow_jump() {
        let (db, engine) = setup();

        let yaml = r##"
name: cross-flow-test
version: 1
flows:
  main:
    steps:
      - id: start
        type: step
        label: "Start"
        agent: atlas
        prompt: "Start"
      - id: go
        type: jump
        label: "Jump to other"
        target: flow other
  other:
    color: "#f97316"
    steps:
      - id: other_step
        type: step
        label: "Other Step"
        agent: atlas
        prompt: "In other flow"
"##;
        let wf_id = create_workflow(&db, yaml);
        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Complete the start step
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task_id, "completed", "started")
            .unwrap();

        // Should have jumped to the other flow
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_flow, "other");
        assert_eq!(instance.current_step_index, 0);

        // other_step should be running
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert!(execs
            .iter()
            .any(|e| e.step_id == "other_step" && e.status == "running"));
    }
}
