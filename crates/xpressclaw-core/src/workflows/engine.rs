use std::sync::Arc;

use serde_json::Value;
use tracing::{error, info, warn};

use crate::db::Database;
use crate::error::{Error, Result};
use crate::tasks::board::{CreateTask, TaskBoard};
use crate::tasks::queue::TaskQueue;

use super::condition;
use super::context;
use super::definition::{WorkflowDefinition, WorkflowNode, WorkflowTrigger};
use super::instance::{InstanceManager, NodeExecution};
use super::manager::WorkflowManager;

/// Maximum number of times a single node can be re-executed within one
/// workflow instance. Prevents infinite cycles.
const MAX_CYCLES: i32 = 10;

/// The workflow runtime engine.
///
/// Manages the lifecycle of workflow instances: starting them from triggers,
/// advancing nodes when tasks complete, evaluating edge conditions, and
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

        let instance =
            self.instances
                .create_instance(workflow_id, definition.version, Some(&trigger_json))?;

        info!(
            workflow_id,
            instance_id = instance.id.as_str(),
            "started workflow instance"
        );

        // Find entry nodes and advance to each
        let entry_nodes: Vec<WorkflowNode> =
            definition.entry_nodes().into_iter().cloned().collect();
        if entry_nodes.is_empty() {
            self.instances.update_instance_status(
                &instance.id,
                "failed",
                Some("no entry nodes found"),
            )?;
            return Err(Error::Workflow("workflow has no entry nodes".into()));
        }

        // Collect completed node outputs (empty at start)
        let node_outputs: Vec<(String, String)> = Vec::new();

        for node in &entry_nodes {
            self.advance_to_node(
                &instance.id,
                node,
                &definition,
                &trigger_data,
                &node_outputs,
            )?;
        }

        Ok(instance.id)
    }

    /// Advance the workflow to execute a specific node.
    fn advance_to_node(
        &self,
        instance_id: &str,
        node: &WorkflowNode,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        node_outputs: &[(String, String)],
    ) -> Result<()> {
        // Cycle guard
        let attempts = self
            .instances
            .get_node_attempt_count(instance_id, &node.id)?;
        if attempts >= MAX_CYCLES {
            let msg = format!("node '{}' exceeded max cycles ({})", node.id, MAX_CYCLES);
            error!(instance_id, node_id = node.id.as_str(), "{}", msg);
            self.instances
                .update_instance_status(instance_id, "failed", Some(&msg))?;
            return Err(Error::Workflow(msg));
        }

        // Build context
        let ctx = context::build_context(trigger_data, node_outputs);
        let ctx_json = serde_json::to_string(&ctx).unwrap_or_else(|_| "{}".to_string());

        let node_type = node.node_type.as_deref().unwrap_or("task");

        // Router/branch nodes evaluate conditions immediately without creating a task.
        // They look at the previous node's output and route to the appropriate edge.
        if node_type == "router" || node_type == "branch" {
            let exec =
                self.instances
                    .create_node_execution(instance_id, &node.id, Some(&ctx_json))?;
            self.instances.set_current_node(instance_id, &node.id)?;

            // Use the last completed node's output as the "task output" for condition evaluation
            let last_output = node_outputs.last().map(|(_, o)| o.as_str()).unwrap_or("");

            self.instances
                .update_node_status(&exec.id, "completed", Some("routed"))?;

            info!(
                instance_id,
                node_id = node.id.as_str(),
                "router node evaluating conditions"
            );

            self.check_completion_after_node(
                instance_id,
                &node.id,
                "completed",
                last_output,
                definition,
                trigger_data,
                node_outputs,
            )?;
            return Ok(());
        }

        if node_type == "sink" {
            // Sink nodes don't create tasks — they deliver messages.
            // Create a node execution and immediately mark it completed.
            let exec =
                self.instances
                    .create_node_execution(instance_id, &node.id, Some(&ctx_json))?;

            // Deliver sink messages (log for now; actual delivery will be
            // handled by the connector registry integration).
            for sink in &node.sinks {
                let rendered = match &sink.template {
                    Some(tmpl) => context::render_template(tmpl, &ctx),
                    None => format!("Workflow node '{}' completed", node.id),
                };
                info!(
                    instance_id,
                    node_id = node.id.as_str(),
                    connector = sink.connector.as_str(),
                    channel = sink.channel.as_str(),
                    message = rendered.as_str(),
                    "sink message ready for delivery"
                );
            }

            self.instances
                .update_node_status(&exec.id, "completed", Some("sink delivered"))?;
            self.instances.set_current_node(instance_id, &node.id)?;

            // Check if workflow is done by evaluating outgoing edges
            self.check_completion_after_node(
                instance_id,
                &node.id,
                "completed",
                "sink delivered",
                definition,
                trigger_data,
                node_outputs,
            )?;

            return Ok(());
        }

        // Task node — render prompt and create a task
        let rendered_prompt = match &node.prompt {
            Some(tmpl) => context::render_template(tmpl, &ctx),
            None => format!("Execute workflow node: {}", node.id),
        };

        let label = node.label.as_deref().unwrap_or(&node.id).to_string();

        let board = TaskBoard::new(self.db.clone());
        let task = board.create(&CreateTask {
            title: label,
            description: Some(rendered_prompt),
            agent_id: node.agent.clone(),
            parent_task_id: None,
            sop_id: node.procedure.clone(),
            conversation_id: None,
            priority: None,
            context: None,
        })?;

        // Enqueue for the dispatcher
        if let Some(ref agent_id) = node.agent {
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

        // Create node execution and link to task
        let exec = self
            .instances
            .create_node_execution(instance_id, &node.id, Some(&ctx_json))?;
        self.instances.set_node_task(&exec.id, &task.id)?;
        self.instances.set_current_node(instance_id, &node.id)?;

        info!(
            instance_id,
            node_id = node.id.as_str(),
            task_id = task.id.as_str(),
            "advanced workflow to node"
        );

        Ok(())
    }

    /// Called by the task dispatcher after a task completes.
    ///
    /// Evaluates outgoing edge conditions and advances to the next node,
    /// or marks the workflow instance as completed if this is a terminal node.
    pub fn on_task_completed(
        &self,
        task_id: &str,
        task_status: &str,
        task_output: &str,
    ) -> Result<()> {
        // Find the node execution for this task
        let exec = match self.instances.find_execution_by_task(task_id)? {
            Some(e) => e,
            None => return Ok(()), // Not part of a workflow
        };

        // Map task status to node status
        let node_status = match task_status {
            "completed" => "completed",
            "cancelled" => "failed",
            _ => "failed",
        };

        self.instances
            .update_node_status(&exec.id, node_status, Some(task_output))?;

        // Load workflow definition
        let instance = self.instances.get_instance(&exec.instance_id)?;
        let record = self.manager.get(&instance.workflow_id)?;
        let definition = WorkflowDefinition::parse(&record.yaml_content)?;

        // Gather trigger data
        let trigger_data: Value = instance
            .trigger_data
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);

        // Gather all completed node outputs for context
        let node_outputs = self.collect_node_outputs(&exec.instance_id)?;

        self.check_completion_after_node(
            &exec.instance_id,
            &exec.node_id,
            task_status,
            task_output,
            &definition,
            &trigger_data,
            &node_outputs,
        )?;

        Ok(())
    }

    /// After a node completes, evaluate outgoing edges and advance or complete.
    #[allow(clippy::too_many_arguments)]
    fn check_completion_after_node(
        &self,
        instance_id: &str,
        node_id: &str,
        task_status: &str,
        task_output: &str,
        definition: &WorkflowDefinition,
        trigger_data: &Value,
        node_outputs: &[(String, String)],
    ) -> Result<()> {
        let outgoing = definition.outgoing_edges(node_id);

        if outgoing.is_empty() {
            // Terminal node — workflow is done
            info!(
                instance_id,
                node_id, "workflow reached terminal node, completing"
            );
            self.instances.complete_instance(instance_id)?;
            return Ok(());
        }

        // Evaluate conditions, take the first match.
        // "default" is tried last as a fallback.
        let mut default_edge = None;
        let mut matched_edge = None;

        for edge in &outgoing {
            let cond = condition::parse(&edge.condition)?;
            if matches!(cond, condition::Condition::Default) {
                default_edge = Some(edge);
                continue;
            }
            if condition::evaluate(&cond, task_status, task_output) {
                matched_edge = Some(edge);
                break;
            }
        }

        let edge = matched_edge.or(default_edge);

        match edge {
            Some(e) => {
                let target_node = definition.node_by_id(&e.to).ok_or_else(|| {
                    Error::Workflow(format!("edge target node '{}' not found", e.to))
                })?;

                info!(
                    instance_id,
                    from = node_id,
                    to = e.to.as_str(),
                    condition = e.condition.as_str(),
                    "advancing workflow along edge"
                );

                self.advance_to_node(
                    instance_id,
                    target_node,
                    definition,
                    trigger_data,
                    node_outputs,
                )?;
            }
            None => {
                // No matching edge — treat as terminal
                info!(
                    instance_id,
                    node_id, "no matching edge, completing workflow"
                );
                self.instances.complete_instance(instance_id)?;
            }
        }

        Ok(())
    }

    /// Collect all completed node outputs for an instance, for context building.
    fn collect_node_outputs(&self, instance_id: &str) -> Result<Vec<(String, String)>> {
        let execs = self.instances.list_node_executions(instance_id)?;
        Ok(execs
            .into_iter()
            .filter(|e| e.status == "completed" && e.output.is_some())
            .map(|e| (e.node_id, e.output.unwrap_or_default()))
            .collect())
    }

    /// Find a node execution by task ID. Delegates to InstanceManager.
    pub fn find_execution_by_task(&self, task_id: &str) -> Result<Option<NodeExecution>> {
        self.instances.find_execution_by_task(task_id)
    }

    /// Recover running workflow instances after a restart.
    ///
    /// For each running instance, check if the current node's task completed
    /// while the server was down. If so, process the completion.
    pub fn recover(&self) -> Result<()> {
        let running = self.instances.list_running_instances()?;
        if running.is_empty() {
            return Ok(());
        }

        info!(count = running.len(), "recovering workflow instances");

        let board = TaskBoard::new(self.db.clone());

        for instance in &running {
            if let Some(ref current_node_id) = instance.current_node_id {
                // Find the node execution for the current node
                let execs = self.instances.list_node_executions(&instance.id)?;
                let current_exec = execs
                    .iter()
                    .rfind(|e| e.node_id == *current_node_id && e.status == "running");

                if let Some(exec) = current_exec {
                    if let Some(ref task_id) = exec.task_id {
                        // Check if the task completed
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
                                    // Get the task output (last message or empty)
                                    let output = task.description.as_deref().unwrap_or("");
                                    if let Err(e) = self.on_task_completed(task_id, status, output)
                                    {
                                        error!(
                                            instance_id = instance.id.as_str(),
                                            task_id = task_id.as_str(),
                                            error = %e,
                                            "failed to recover workflow task"
                                        );
                                    }
                                }
                                // If task is still running, leave it — dispatcher will complete it
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
        }

        Ok(())
    }

    /// Process unprocessed connector events, starting workflow instances
    /// for matching triggers.
    ///
    /// Returns the number of events processed.
    pub fn process_events(&self) -> Result<u32> {
        // Query unprocessed events
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

        // Load all enabled workflows
        let workflows = self.manager.list()?;
        let enabled_workflows: Vec<_> = workflows.iter().filter(|w| w.enabled).collect();

        let mut count = 0u32;

        for (event_id, connector_id, channel_id, event_type, payload_str) in &events {
            let payload: Value = serde_json::from_str(payload_str).unwrap_or(Value::Null);

            // Find matching workflows
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

            // Mark event as processed
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
}

/// Check if a workflow trigger matches an incoming connector event.
///
/// Matches on connector name/ID, channel name/ID, and event type.
/// Filter conditions are checked against the payload.
pub fn matches_trigger(
    trigger: &WorkflowTrigger,
    event_connector: &str,
    event_channel: &str,
    event_type: &str,
    event_payload: &Value,
) -> bool {
    // Match connector (by name or ID)
    if trigger.connector != event_connector {
        return false;
    }

    // Match channel (by name or ID)
    if trigger.channel != event_channel {
        return false;
    }

    // Match event type
    if trigger.event != event_type {
        return false;
    }

    // Check filter conditions
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
nodes:
  - id: step1
    label: "First Step"
    agent: atlas
    prompt: "Do step 1: {{trigger.payload.summary}}"
  - id: step2
    label: "Second Step"
    agent: atlas
    prompt: "Do step 2 based on: {{nodes.step1.output}}"
edges:
  - from: step1
    to: step2
    condition: completed
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
        assert_eq!(instance.current_node_id.as_deref(), Some("step1"));

        // Node execution should exist with a task
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0].node_id, "step1");
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

        // Get the task ID from the first node execution
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        // Complete the task
        engine
            .on_task_completed(&task_id, "completed", "Step 1 output")
            .unwrap();

        // Should have advanced to step2
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_node_id.as_deref(), Some("step2"));

        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 2);
        assert_eq!(execs[1].node_id, "step2");
    }

    #[test]
    fn test_terminal_node_completes_workflow() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({"summary": "Test"}))
            .unwrap();

        // Complete step1
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task1_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task1_id, "completed", "output1")
            .unwrap();

        // Complete step2 (terminal node)
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
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
        // Should return Ok(()) for tasks not in any workflow
        engine
            .on_task_completed("nonexistent-task", "completed", "output")
            .unwrap();
    }

    #[test]
    fn test_cyclic_workflow() {
        let yaml = r#"
name: cyclic-test
version: 1
nodes:
  - id: start
    label: "Start"
    agent: atlas
    prompt: "Begin"
  - id: a
    label: "Step A"
    agent: atlas
    prompt: "Do A"
  - id: b
    label: "Step B"
    agent: atlas
    prompt: "Do B"
edges:
  - from: start
    to: a
    condition: completed
  - from: a
    to: b
    condition: completed
  - from: b
    to: a
    condition: "output contains \"retry\""
  - from: b
    to: b
    condition: default
"#;
        // start -> a -> b, then b can cycle back to a if output contains "retry"

        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Complete start
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let start_task_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&start_task_id, "completed", "started")
            .unwrap();

        // Should have advanced to a
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_node_id.as_deref(), Some("a"));

        // Complete a
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task_a_id = execs
            .iter()
            .find(|e| e.node_id == "a")
            .unwrap()
            .task_id
            .as_ref()
            .unwrap()
            .clone();
        engine
            .on_task_completed(&task_a_id, "completed", "done")
            .unwrap();

        // Should have advanced to b
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_node_id.as_deref(), Some("b"));

        // Complete b with "retry" output — should cycle back to a
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task_b_id = execs
            .iter()
            .rev()
            .find(|e| e.node_id == "b")
            .unwrap()
            .task_id
            .as_ref()
            .unwrap()
            .clone();
        engine
            .on_task_completed(&task_b_id, "completed", "please retry")
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_node_id.as_deref(), Some("a"));
    }

    #[test]
    fn test_conditional_branching() {
        let yaml = r#"
name: branching-test
version: 1
nodes:
  - id: review
    label: "Review"
    agent: atlas
    prompt: "Review this"
  - id: approve
    label: "Approve"
    agent: atlas
    prompt: "Approved"
  - id: reject
    label: "Reject"
    agent: atlas
    prompt: "Rejected"
edges:
  - from: review
    to: approve
    condition: "output.verdict == \"pass\""
  - from: review
    to: reject
    condition: "output.verdict == \"fail\""
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        // Test pass path
        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        engine
            .on_task_completed(&task_id, "completed", r#"{"verdict": "pass"}"#)
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_node_id.as_deref(), Some("approve"));
    }

    #[test]
    fn test_sink_node() {
        let yaml = r#"
name: sink-test
version: 1
nodes:
  - id: work
    label: "Do Work"
    agent: atlas
    prompt: "Work"
  - id: notify
    type: sink
    label: "Notify"
    sinks:
      - connector: telegram
        channel: dev-chat
        template: "Done: {{nodes.work.output}}"
edges:
  - from: work
    to: notify
    condition: completed
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        // Complete the work node
        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();
        engine
            .on_task_completed(&task_id, "completed", "work output")
            .unwrap();

        // Sink node should be completed and workflow should be done
        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.status, "completed");

        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        assert_eq!(execs.len(), 2);
        assert_eq!(execs[1].node_id, "notify");
        assert_eq!(execs[1].status, "completed");
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

        assert!(!matches_trigger(
            &trigger,
            "jira",
            "my-project",
            "issue_created",
            &serde_json::json!({})
        ));
    }

    #[test]
    fn test_process_events() {
        let (db, engine) = setup();

        // Create a workflow with a trigger
        let yaml = r#"
name: event-workflow
version: 1
trigger:
  connector: webhook
  channel: incoming
  event: message
nodes:
  - id: handle
    label: "Handle"
    agent: atlas
    prompt: "Handle: {{trigger.payload.text}}"
edges: []
"#;
        let wf_id = create_workflow(&db, yaml);
        // Make sure it's enabled (it is by default)
        let _ = wf_id;

        // Insert an unprocessed event
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

        // Event should now be processed
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
        // Should succeed with no running instances
        engine.recover().unwrap();
    }

    #[test]
    fn test_find_execution_by_task() {
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, SIMPLE_WORKFLOW);

        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap();

        let found = engine.find_execution_by_task(task_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().node_id, "step1");

        let not_found = engine.find_execution_by_task("no-such-task").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_default_edge_fallback() {
        let yaml = r#"
name: default-fallback
version: 1
nodes:
  - id: check
    label: "Check"
    agent: atlas
    prompt: "Check"
  - id: success
    label: "Success"
    agent: atlas
    prompt: "Success path"
  - id: fallback
    label: "Fallback"
    agent: atlas
    prompt: "Fallback path"
edges:
  - from: check
    to: success
    condition: "output.status == \"ok\""
  - from: check
    to: fallback
    condition: default
"#;
        let (db, engine) = setup();
        let wf_id = create_workflow(&db, yaml);

        // Output doesn't match "ok", so should fall through to default
        let instance_id = engine
            .start_instance(&wf_id, serde_json::json!({}))
            .unwrap();

        let execs = engine.instances.list_node_executions(&instance_id).unwrap();
        let task_id = execs[0].task_id.as_ref().unwrap().clone();

        engine
            .on_task_completed(&task_id, "completed", r#"{"status": "error"}"#)
            .unwrap();

        let instance = engine.instances.get_instance(&instance_id).unwrap();
        assert_eq!(instance.current_node_id.as_deref(), Some("fallback"));
    }
}
