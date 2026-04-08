use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tracing::{error, info, warn};

use crate::db::Database;
use crate::error::{Error, Result};
use crate::tasks::board::{CreateTask, TaskBoard};
use crate::tasks::queue::TaskQueue;

use super::condition;
use super::context;
use super::definition::{Step, WorkflowDefinition, WorkflowTrigger};
use super::instance::{InstanceManager, StepExecution};
use super::manager::WorkflowManager;

/// Maximum number of times a single step can be re-executed within one
/// workflow instance. Prevents infinite cycles.
const MAX_CYCLES: i32 = 10;

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
        let record = self.manager.get(workflow_id)?;
        let definition = WorkflowDefinition::parse(&record.yaml_content)?;

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

        let instance = self.instances.create_instance(
            workflow_id,
            Some(&trigger_json),
            vars_json.as_deref(),
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

        Ok(instance.id)
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
            "step" => self.execute_task_step(instance_id, flow_name, step, &ctx, &ctx_json),
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
    ) -> Result<()> {
        let rendered_prompt = match &step.prompt {
            Some(tmpl) => context::render_template(tmpl, ctx),
            None => format!("Execute workflow step: {}", step.id),
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
            agent_id: step.agent.clone(),
            parent_task_id: None,
            sop_id: step.procedure.clone(),
            conversation_id: None,
            priority: None,
            context: None,
        })?;

        // Enqueue for the dispatcher
        if let Some(ref agent_id) = step.agent {
            let queue = TaskQueue::new(self.db.clone());
            if let Err(e) = queue.enqueue(&task.id, agent_id) {
                warn!(
                    task_id = task.id.as_str(),
                    agent_id = agent_id.as_str(),
                    error = %e,
                    "failed to enqueue workflow task"
                );
            }
        }

        // Create step execution and link to task
        let exec = self.instances.create_step_execution(
            instance_id,
            flow_name,
            &step.id,
            Some(ctx_json),
        )?;
        self.instances.set_step_task(&exec.id, &task.id)?;

        info!(
            instance_id,
            flow_name,
            step_id = step.id.as_str(),
            task_id = task.id.as_str(),
            "executing task step"
        );

        // Wait for dispatcher to call on_task_completed
        Ok(())
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

    /// Execute a loop step: iterate over array, run nested steps for each item.
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
        let over_expr = step.over.as_deref().unwrap_or("");
        let as_var = step.as_var.as_deref().unwrap_or("item");

        let items_value = context::resolve_variable(over_expr, ctx).unwrap_or(Value::Array(vec![]));

        let items = match items_value {
            Value::Array(arr) => arr,
            _ => vec![items_value],
        };

        info!(
            instance_id,
            step_id = step.id.as_str(),
            item_count = items.len(),
            "executing loop step"
        );

        let ctx_json = serde_json::to_string(ctx).unwrap_or_else(|_| "{}".to_string());
        let exec = self.instances.create_step_execution(
            instance_id,
            flow_name,
            &step.id,
            Some(&ctx_json),
        )?;

        let body_steps = match &step.body {
            Some(b) => b,
            None => {
                self.instances
                    .update_step_status(&exec.id, "completed", None)?;
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

        // For each item, set the loop var and execute body steps synchronously.
        // Note: Since task steps are async (wait for dispatcher), loops with
        // task steps will only execute the first item's first task step, then
        // the loop must be resumed via on_task_completed. For simplicity in v2,
        // we only support loops with non-task steps (sink, when, jump) executing
        // synchronously. Task steps in loops will execute only the first one.
        let mut var_store = variable_store.clone();

        for (idx, item) in items.iter().enumerate() {
            var_store.insert(as_var.to_string(), item.clone());

            // Save the loop state
            let loop_state = serde_json::json!({
                "step_id": step.id,
                "flow_name": flow_name,
                "step_index": step_index,
                "current_item": idx,
                "total_items": items.len(),
                "as_var": as_var,
            });
            self.instances.update_loop_state(
                instance_id,
                Some(&serde_json::to_string(&loop_state).unwrap_or_default()),
            )?;
            self.save_variable_store(instance_id, &var_store)?;

            // Execute body steps (only non-async steps for now)
            for body_step in body_steps {
                match body_step.step_type.as_str() {
                    "sink" => {
                        let body_ctx =
                            context::build_context(trigger_data, &definition.variables, &var_store);
                        let body_ctx_json =
                            serde_json::to_string(&body_ctx).unwrap_or_else(|_| "{}".to_string());
                        let body_exec = self.instances.create_step_execution(
                            instance_id,
                            flow_name,
                            &body_step.id,
                            Some(&body_ctx_json),
                        )?;

                        if let Some(ref sinks) = body_step.sinks {
                            for sink in sinks {
                                let rendered = match &sink.template {
                                    Some(tmpl) => context::render_template(tmpl, &body_ctx),
                                    None => format!("Loop iteration {} of step '{}'", idx, step.id),
                                };
                                crate::connectors::deliver::deliver(
                                    &self.db,
                                    &sink.connector,
                                    &sink.channel,
                                    &rendered,
                                );
                            }
                        }
                        self.instances.update_step_status(
                            &body_exec.id,
                            "completed",
                            Some("sink delivered"),
                        )?;
                    }
                    "step" => {
                        // Task step in loop: execute as a regular task step.
                        // The loop will not continue automatically; it would need
                        // to be resumed by on_task_completed. For now, we execute
                        // the first task and return (loop continues on completion).
                        let body_ctx =
                            context::build_context(trigger_data, &definition.variables, &var_store);
                        let body_ctx_json =
                            serde_json::to_string(&body_ctx).unwrap_or_else(|_| "{}".to_string());
                        self.execute_task_step(
                            instance_id,
                            flow_name,
                            body_step,
                            &body_ctx,
                            &body_ctx_json,
                        )?;
                        // Return here — task is async, will resume via on_task_completed
                        return Ok(());
                    }
                    _ => {
                        // Other step types in loops — skip for now
                        warn!(
                            step_type = body_step.step_type.as_str(),
                            "unsupported step type in loop body, skipping"
                        );
                    }
                }
            }
        }

        // Loop completed
        self.instances.update_loop_state(instance_id, None)?;
        self.instances
            .update_step_status(&exec.id, "completed", None)?;

        self.advance_to_next(
            instance_id,
            flow_name,
            step_index,
            definition,
            trigger_data,
            &var_store,
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
            ["flow", flow_name] => self.execute_step(
                instance_id,
                flow_name,
                0,
                definition,
                trigger_data,
                variable_store,
            ),
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

        let record = self.manager.get(&instance.workflow_id)?;
        let definition = WorkflowDefinition::parse(&record.yaml_content)?;

        // Gather trigger data
        let trigger_data: Value = instance
            .trigger_data
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);

        // Load and update variable store
        let mut var_store = self.load_variable_store(&exec.instance_id)?;

        // Try to parse output as JSON and store under step_id
        let output_value = serde_json::from_str::<Value>(task_output)
            .unwrap_or_else(|_| Value::String(task_output.to_string()));

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
                // No declared outputs — store raw
                var_store.insert(
                    exec.step_id.clone(),
                    serde_json::json!({ "output": output_value }),
                );
            }
        } else {
            var_store.insert(
                exec.step_id.clone(),
                serde_json::json!({ "output": output_value }),
            );
        }

        self.save_variable_store(&exec.instance_id, &var_store)?;

        // Check if this was a task inside a loop
        if let Some(ref loop_state_str) = instance.loop_state {
            if let Ok(loop_state) = serde_json::from_str::<Value>(loop_state_str) {
                // There's an active loop — handle resumption
                // For now, complete the instance normally by advancing
                // (full loop resumption would need more state tracking)
                let _ = loop_state; // Acknowledged but not fully resumed in v2 MVP
            }
        }

        if step_status == "failed" {
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

    /// Find a step execution by task ID.
    pub fn find_execution_by_task(&self, task_id: &str) -> Result<Option<StepExecution>> {
        self.instances.find_execution_by_task(task_id)
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
                                let output = task.description.as_deref().unwrap_or("");
                                if let Err(e) = self.on_task_completed(task_id, status, output) {
                                    error!(
                                        instance_id = instance.id.as_str(),
                                        task_id = task_id.as_str(),
                                        error = %e,
                                        "failed to recover workflow task"
                                    );
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
                }
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

        // Step execution should exist with a task
        let execs = engine.instances.list_step_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].step_id, "step1");
        assert_eq!(execs[0].flow_name, "main");
        assert_eq!(execs[0].status, "running");
        assert!(execs[0].task_id.is_some());
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
}
