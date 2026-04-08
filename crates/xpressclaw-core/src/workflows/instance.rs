use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};

/// A running (or completed) workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowInstance {
    pub id: String,
    pub workflow_id: String,
    pub status: String, // running, completed, failed, cancelled
    pub current_flow: String,
    pub current_step_index: i32,
    pub trigger_data: Option<String>, // JSON
    pub variable_store: String,       // JSON
    pub loop_state: Option<String>,   // JSON
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error_message: Option<String>,
}

/// A single step execution within a workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecution {
    pub id: String,
    pub instance_id: String,
    pub flow_name: String,
    pub step_id: String,
    pub task_id: Option<String>,
    pub status: String, // pending, running, completed, failed, skipped
    pub input_context: Option<String>,
    pub output: Option<String>,
    pub attempt: i32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Manages workflow instances and step executions in the database.
pub struct InstanceManager {
    db: Arc<Database>,
}

impl InstanceManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Create a new workflow instance.
    pub fn create_instance(
        &self,
        workflow_id: &str,
        trigger_data: Option<&str>,
        variables_json: Option<&str>,
    ) -> Result<WorkflowInstance> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let var_store = variables_json.unwrap_or("{}");

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_instances (id, workflow_id, status, current_flow, current_step_index, trigger_data, variable_store, started_at)
                 VALUES (?1, ?2, 'running', 'main', 0, ?3, ?4, ?5)",
                rusqlite::params![id, workflow_id, trigger_data, var_store, now],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        self.get_instance(&id)
    }

    /// Get a workflow instance by ID.
    pub fn get_instance(&self, id: &str) -> Result<WorkflowInstance> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_instances WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_instance(row)))
                .map_err(|_| Error::WorkflowInstanceNotFound { id: id.to_string() })
        })
    }

    /// List instances for a given workflow, ordered by most recent first.
    pub fn list_instances(&self, workflow_id: &str, limit: i64) -> Result<Vec<WorkflowInstance>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM workflow_instances WHERE workflow_id = ?1 ORDER BY started_at DESC LIMIT ?2",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map(rusqlite::params![workflow_id, limit], |row| {
                    Ok(row_to_instance(row))
                })
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// List all running workflow instances.
    pub fn list_running_instances(&self) -> Result<Vec<WorkflowInstance>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_instances WHERE status = 'running' ORDER BY started_at ASC")
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([], |row| Ok(row_to_instance(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// Update the status of a workflow instance.
    pub fn update_status(&self, id: &str, status: &str, error_msg: Option<&str>) -> Result<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET status = ?1, error_message = ?2, completed_at = ?3 WHERE id = ?4",
                rusqlite::params![status, error_msg, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Mark a workflow instance as completed.
    pub fn complete_instance(&self, id: &str) -> Result<()> {
        self.update_status(id, "completed", None)
    }

    /// Set the current position (flow + step index) of a workflow instance.
    pub fn set_current_position(
        &self,
        instance_id: &str,
        flow: &str,
        step_index: i32,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET current_flow = ?1, current_step_index = ?2 WHERE id = ?3",
                rusqlite::params![flow, step_index, instance_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Get the variable store JSON for an instance.
    pub fn get_variable_store(&self, instance_id: &str) -> Result<String> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT variable_store FROM workflow_instances WHERE id = ?1",
                [instance_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| Error::WorkflowInstanceNotFound {
                id: instance_id.to_string(),
            })
        })
    }

    /// Update the variable store JSON for an instance.
    pub fn update_variable_store(&self, instance_id: &str, store_json: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET variable_store = ?1 WHERE id = ?2",
                rusqlite::params![store_json, instance_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Update the loop state JSON for an instance.
    pub fn update_loop_state(&self, instance_id: &str, loop_state: Option<&str>) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET loop_state = ?1 WHERE id = ?2",
                rusqlite::params![loop_state, instance_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    // -- Step Execution methods --

    /// Create a new step execution record.
    pub fn create_step_execution(
        &self,
        instance_id: &str,
        flow_name: &str,
        step_id: &str,
        input_context: Option<&str>,
    ) -> Result<StepExecution> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let attempt = self.get_step_attempt_count(instance_id, step_id)? + 1;

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_step_executions (id, instance_id, flow_name, step_id, status, input_context, attempt, started_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7)",
                rusqlite::params![id, instance_id, flow_name, step_id, input_context, attempt, now],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        self.get_step_execution(&id)
    }

    /// Get a step execution by ID.
    fn get_step_execution(&self, id: &str) -> Result<StepExecution> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_step_executions WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_step_execution(row)))
                .map_err(|_| Error::Workflow(format!("step execution not found: {id}")))
        })
    }

    /// Find a step execution by its linked task ID.
    pub fn find_execution_by_task(&self, task_id: &str) -> Result<Option<StepExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_step_executions WHERE task_id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            let result = stmt
                .query_row([task_id], |row| Ok(row_to_step_execution(row)))
                .ok();

            Ok(result)
        })
    }

    /// Update the status and output of a step execution.
    pub fn update_step_status(&self, id: &str, status: &str, output: Option<&str>) -> Result<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_step_executions SET status = ?1, output = ?2, completed_at = ?3 WHERE id = ?4",
                rusqlite::params![status, output, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Link a step execution to a task.
    pub fn set_step_task(&self, execution_id: &str, task_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_step_executions SET task_id = ?1, status = 'running' WHERE id = ?2",
                rusqlite::params![task_id, execution_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// List all step executions for a workflow instance.
    pub fn list_step_executions(&self, instance_id: &str) -> Result<Vec<StepExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM workflow_step_executions WHERE instance_id = ?1 ORDER BY started_at ASC",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([instance_id], |row| Ok(row_to_step_execution(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// Get the number of times a step has been executed within an instance.
    pub fn get_step_attempt_count(&self, instance_id: &str, step_id: &str) -> Result<i32> {
        self.db.with_conn(|conn| {
            let count: i32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM workflow_step_executions WHERE instance_id = ?1 AND step_id = ?2",
                    rusqlite::params![instance_id, step_id],
                    |row| row.get(0),
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            Ok(count)
        })
    }
}

fn row_to_instance(row: &rusqlite::Row) -> WorkflowInstance {
    WorkflowInstance {
        id: row.get("id").unwrap_or_default(),
        workflow_id: row.get("workflow_id").unwrap_or_default(),
        status: row.get("status").unwrap_or_default(),
        current_flow: row
            .get("current_flow")
            .unwrap_or_else(|_| "main".to_string()),
        current_step_index: row.get("current_step_index").unwrap_or(0),
        trigger_data: row.get("trigger_data").unwrap_or_default(),
        variable_store: row
            .get("variable_store")
            .unwrap_or_else(|_| "{}".to_string()),
        loop_state: row.get("loop_state").unwrap_or_default(),
        started_at: row.get("started_at").unwrap_or_default(),
        completed_at: row.get("completed_at").unwrap_or_default(),
        error_message: row.get("error_message").unwrap_or_default(),
    }
}

fn row_to_step_execution(row: &rusqlite::Row) -> StepExecution {
    StepExecution {
        id: row.get("id").unwrap_or_default(),
        instance_id: row.get("instance_id").unwrap_or_default(),
        flow_name: row.get("flow_name").unwrap_or_default(),
        step_id: row.get("step_id").unwrap_or_default(),
        task_id: row.get("task_id").unwrap_or_default(),
        status: row.get("status").unwrap_or_default(),
        input_context: row.get("input_context").unwrap_or_default(),
        output: row.get("output").unwrap_or_default(),
        attempt: row.get("attempt").unwrap_or(1),
        started_at: row.get("started_at").unwrap_or_default(),
        completed_at: row.get("completed_at").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, InstanceManager) {
        let db = Arc::new(Database::open_memory().unwrap());

        // Insert a dummy workflow for foreign key
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflows (id, name, yaml_content, version) VALUES ('wf1', 'test', 'yaml', 2)",
                [],
            )
            .unwrap();
        });

        let mgr = InstanceManager::new(db.clone());
        (db, mgr)
    }

    #[test]
    fn test_create_and_get_instance() {
        let (_, mgr) = setup();
        let inst = mgr
            .create_instance("wf1", Some(r#"{"key": "value"}"#), None)
            .unwrap();

        assert_eq!(inst.workflow_id, "wf1");
        assert_eq!(inst.status, "running");
        assert_eq!(inst.current_flow, "main");
        assert_eq!(inst.current_step_index, 0);
        assert_eq!(inst.trigger_data.as_deref(), Some(r#"{"key": "value"}"#));
        assert_eq!(inst.variable_store, "{}");

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.id, inst.id);
    }

    #[test]
    fn test_create_instance_with_variables() {
        let (_, mgr) = setup();
        let inst = mgr
            .create_instance("wf1", None, Some(r#"{"default_agent": "atlas"}"#))
            .unwrap();

        assert_eq!(inst.variable_store, r#"{"default_agent": "atlas"}"#);
    }

    #[test]
    fn test_list_instances() {
        let (_, mgr) = setup();
        mgr.create_instance("wf1", None, None).unwrap();
        mgr.create_instance("wf1", None, None).unwrap();

        let list = mgr.list_instances("wf1", 10).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_running_instances() {
        let (_, mgr) = setup();
        let inst1 = mgr.create_instance("wf1", None, None).unwrap();
        let inst2 = mgr.create_instance("wf1", None, None).unwrap();

        mgr.complete_instance(&inst1.id).unwrap();

        let running = mgr.list_running_instances().unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, inst2.id);
    }

    #[test]
    fn test_update_status() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.update_status(&inst.id, "failed", Some("something broke"))
            .unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.status, "failed");
        assert_eq!(fetched.error_message.as_deref(), Some("something broke"));
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_set_current_position() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.set_current_position(&inst.id, "bug_flow", 2).unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.current_flow, "bug_flow");
        assert_eq!(fetched.current_step_index, 2);
    }

    #[test]
    fn test_variable_store() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        let store = mgr.get_variable_store(&inst.id).unwrap();
        assert_eq!(store, "{}");

        mgr.update_variable_store(&inst.id, r#"{"classify": {"intent": "bug"}}"#)
            .unwrap();

        let store = mgr.get_variable_store(&inst.id).unwrap();
        assert_eq!(store, r#"{"classify": {"intent": "bug"}}"#);
    }

    #[test]
    fn test_loop_state() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        assert!(inst.loop_state.is_none());

        mgr.update_loop_state(&inst.id, Some(r#"{"index": 0, "items": [1,2,3]}"#))
            .unwrap();
        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert!(fetched.loop_state.is_some());

        mgr.update_loop_state(&inst.id, None).unwrap();
        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert!(fetched.loop_state.is_none());
    }

    #[test]
    fn test_create_and_get_step_execution() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", Some(r#"{"ctx": true}"#))
            .unwrap();

        assert_eq!(exec.instance_id, inst.id);
        assert_eq!(exec.flow_name, "main");
        assert_eq!(exec.step_id, "step1");
        assert_eq!(exec.status, "pending");
        assert_eq!(exec.attempt, 1);
        assert_eq!(exec.input_context.as_deref(), Some(r#"{"ctx": true}"#));
    }

    #[test]
    fn test_step_attempt_count() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        assert_eq!(mgr.get_step_attempt_count(&inst.id, "step1").unwrap(), 0);

        mgr.create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();
        assert_eq!(mgr.get_step_attempt_count(&inst.id, "step1").unwrap(), 1);

        mgr.create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();
        assert_eq!(mgr.get_step_attempt_count(&inst.id, "step1").unwrap(), 2);
    }

    #[test]
    fn test_set_step_task() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();

        mgr.set_step_task(&exec.id, "task-123").unwrap();

        let fetched = mgr.get_step_execution(&exec.id).unwrap();
        assert_eq!(fetched.task_id.as_deref(), Some("task-123"));
        assert_eq!(fetched.status, "running");
    }

    #[test]
    fn test_find_execution_by_task() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();

        mgr.set_step_task(&exec.id, "task-456").unwrap();

        let found = mgr.find_execution_by_task("task-456").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, exec.id);

        let not_found = mgr.find_execution_by_task("nonexistent").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_update_step_status() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();
        let exec = mgr
            .create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();

        mgr.update_step_status(&exec.id, "completed", Some("output text"))
            .unwrap();

        let fetched = mgr.get_step_execution(&exec.id).unwrap();
        assert_eq!(fetched.status, "completed");
        assert_eq!(fetched.output.as_deref(), Some("output text"));
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_list_step_executions() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.create_step_execution(&inst.id, "main", "step1", None)
            .unwrap();
        mgr.create_step_execution(&inst.id, "main", "step2", None)
            .unwrap();

        let execs = mgr.list_step_executions(&inst.id).unwrap();
        assert_eq!(execs.len(), 2);
    }

    #[test]
    fn test_complete_instance() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", None, None).unwrap();

        mgr.complete_instance(&inst.id).unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.status, "completed");
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_get_instance_not_found() {
        let (_, mgr) = setup();
        assert!(matches!(
            mgr.get_instance("nonexistent"),
            Err(Error::WorkflowInstanceNotFound { .. })
        ));
    }
}
