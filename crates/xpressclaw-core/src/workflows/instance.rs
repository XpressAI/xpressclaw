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
    pub workflow_version: u32,
    pub status: String,               // running, completed, failed, cancelled
    pub trigger_data: Option<String>, // JSON
    pub current_node_id: Option<String>,
    pub context: String, // JSON
    pub started_at: String,
    pub completed_at: Option<String>,
    pub error_message: Option<String>,
}

/// A single node execution within a workflow instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeExecution {
    pub id: String,
    pub instance_id: String,
    pub node_id: String,
    pub task_id: Option<String>,
    pub status: String, // pending, running, completed, failed, skipped
    pub input_context: Option<String>,
    pub output: Option<String>,
    pub attempt: i32,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Manages workflow instances and node executions in the database.
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
        version: u32,
        trigger_data: Option<&str>,
    ) -> Result<WorkflowInstance> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_instances (id, workflow_id, workflow_version, status, trigger_data, context, started_at)
                 VALUES (?1, ?2, ?3, 'running', ?4, '{}', ?5)",
                rusqlite::params![id, workflow_id, version, trigger_data, now],
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
    pub fn update_instance_status(
        &self,
        id: &str,
        status: &str,
        error_msg: Option<&str>,
    ) -> Result<()> {
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

    /// Set the current node being executed in a workflow instance.
    pub fn set_current_node(&self, instance_id: &str, node_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_instances SET current_node_id = ?1 WHERE id = ?2",
                rusqlite::params![node_id, instance_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Create a new node execution record.
    pub fn create_node_execution(
        &self,
        instance_id: &str,
        node_id: &str,
        input_context: Option<&str>,
    ) -> Result<NodeExecution> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let attempt = self.get_node_attempt_count(instance_id, node_id)? + 1;

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflow_node_executions (id, instance_id, node_id, status, input_context, attempt, started_at)
                 VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6)",
                rusqlite::params![id, instance_id, node_id, input_context, attempt, now],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        self.get_node_execution(&id)
    }

    /// Get a node execution by ID.
    pub fn get_node_execution(&self, id: &str) -> Result<NodeExecution> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_node_executions WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_node_execution(row)))
                .map_err(|_| Error::Workflow(format!("node execution not found: {id}")))
        })
    }

    /// Find a node execution by its linked task ID.
    pub fn find_execution_by_task(&self, task_id: &str) -> Result<Option<NodeExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflow_node_executions WHERE task_id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            let result = stmt
                .query_row([task_id], |row| Ok(row_to_node_execution(row)))
                .ok();

            Ok(result)
        })
    }

    /// Update the status and output of a node execution.
    pub fn update_node_status(&self, id: &str, status: &str, output: Option<&str>) -> Result<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_node_executions SET status = ?1, output = ?2, completed_at = ?3 WHERE id = ?4",
                rusqlite::params![status, output, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// Link a node execution to a task.
    pub fn set_node_task(&self, execution_id: &str, task_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflow_node_executions SET task_id = ?1, status = 'running' WHERE id = ?2",
                rusqlite::params![task_id, execution_id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(())
    }

    /// List all node executions for a workflow instance.
    pub fn list_node_executions(&self, instance_id: &str) -> Result<Vec<NodeExecution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM workflow_node_executions WHERE instance_id = ?1 ORDER BY started_at ASC",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([instance_id], |row| Ok(row_to_node_execution(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// Get the number of times a node has been executed within an instance.
    pub fn get_node_attempt_count(&self, instance_id: &str, node_id: &str) -> Result<i32> {
        self.db.with_conn(|conn| {
            let count: i32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM workflow_node_executions WHERE instance_id = ?1 AND node_id = ?2",
                    rusqlite::params![instance_id, node_id],
                    |row| row.get(0),
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            Ok(count)
        })
    }

    /// Mark a workflow instance as completed.
    pub fn complete_instance(&self, id: &str) -> Result<()> {
        self.update_instance_status(id, "completed", None)
    }
}

fn row_to_instance(row: &rusqlite::Row) -> WorkflowInstance {
    WorkflowInstance {
        id: row.get("id").unwrap_or_default(),
        workflow_id: row.get("workflow_id").unwrap_or_default(),
        workflow_version: row.get::<_, u32>("workflow_version").unwrap_or(1),
        status: row.get("status").unwrap_or_default(),
        trigger_data: row.get("trigger_data").unwrap_or_default(),
        current_node_id: row.get("current_node_id").unwrap_or_default(),
        context: row.get("context").unwrap_or_else(|_| "{}".to_string()),
        started_at: row.get("started_at").unwrap_or_default(),
        completed_at: row.get("completed_at").unwrap_or_default(),
        error_message: row.get("error_message").unwrap_or_default(),
    }
}

fn row_to_node_execution(row: &rusqlite::Row) -> NodeExecution {
    NodeExecution {
        id: row.get("id").unwrap_or_default(),
        instance_id: row.get("instance_id").unwrap_or_default(),
        node_id: row.get("node_id").unwrap_or_default(),
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
                "INSERT INTO workflows (id, name, yaml_content, version) VALUES ('wf1', 'test', 'yaml', 1)",
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
            .create_instance("wf1", 1, Some(r#"{"key": "value"}"#))
            .unwrap();

        assert_eq!(inst.workflow_id, "wf1");
        assert_eq!(inst.workflow_version, 1);
        assert_eq!(inst.status, "running");
        assert_eq!(inst.trigger_data.as_deref(), Some(r#"{"key": "value"}"#));

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.id, inst.id);
    }

    #[test]
    fn test_list_instances() {
        let (_, mgr) = setup();
        mgr.create_instance("wf1", 1, None).unwrap();
        mgr.create_instance("wf1", 1, None).unwrap();

        let list = mgr.list_instances("wf1", 10).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_list_running_instances() {
        let (_, mgr) = setup();
        let inst1 = mgr.create_instance("wf1", 1, None).unwrap();
        let inst2 = mgr.create_instance("wf1", 1, None).unwrap();

        // Complete one
        mgr.complete_instance(&inst1.id).unwrap();

        let running = mgr.list_running_instances().unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, inst2.id);
    }

    #[test]
    fn test_update_instance_status() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();

        mgr.update_instance_status(&inst.id, "failed", Some("something broke"))
            .unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.status, "failed");
        assert_eq!(fetched.error_message.as_deref(), Some("something broke"));
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_set_current_node() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();

        mgr.set_current_node(&inst.id, "step1").unwrap();

        let fetched = mgr.get_instance(&inst.id).unwrap();
        assert_eq!(fetched.current_node_id.as_deref(), Some("step1"));
    }

    #[test]
    fn test_create_and_get_node_execution() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();

        let exec = mgr
            .create_node_execution(&inst.id, "step1", Some(r#"{"ctx": true}"#))
            .unwrap();

        assert_eq!(exec.instance_id, inst.id);
        assert_eq!(exec.node_id, "step1");
        assert_eq!(exec.status, "pending");
        assert_eq!(exec.attempt, 1);
        assert_eq!(exec.input_context.as_deref(), Some(r#"{"ctx": true}"#));

        let fetched = mgr.get_node_execution(&exec.id).unwrap();
        assert_eq!(fetched.id, exec.id);
    }

    #[test]
    fn test_node_attempt_count() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();

        assert_eq!(mgr.get_node_attempt_count(&inst.id, "step1").unwrap(), 0);

        mgr.create_node_execution(&inst.id, "step1", None).unwrap();
        assert_eq!(mgr.get_node_attempt_count(&inst.id, "step1").unwrap(), 1);

        mgr.create_node_execution(&inst.id, "step1", None).unwrap();
        assert_eq!(mgr.get_node_attempt_count(&inst.id, "step1").unwrap(), 2);
    }

    #[test]
    fn test_set_node_task() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();
        let exec = mgr.create_node_execution(&inst.id, "step1", None).unwrap();

        mgr.set_node_task(&exec.id, "task-123").unwrap();

        let fetched = mgr.get_node_execution(&exec.id).unwrap();
        assert_eq!(fetched.task_id.as_deref(), Some("task-123"));
        assert_eq!(fetched.status, "running");
    }

    #[test]
    fn test_find_execution_by_task() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();
        let exec = mgr.create_node_execution(&inst.id, "step1", None).unwrap();

        mgr.set_node_task(&exec.id, "task-456").unwrap();

        let found = mgr.find_execution_by_task("task-456").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, exec.id);

        // Not found case
        let not_found = mgr.find_execution_by_task("nonexistent").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_update_node_status() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();
        let exec = mgr.create_node_execution(&inst.id, "step1", None).unwrap();

        mgr.update_node_status(&exec.id, "completed", Some("output text"))
            .unwrap();

        let fetched = mgr.get_node_execution(&exec.id).unwrap();
        assert_eq!(fetched.status, "completed");
        assert_eq!(fetched.output.as_deref(), Some("output text"));
        assert!(fetched.completed_at.is_some());
    }

    #[test]
    fn test_list_node_executions() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();

        mgr.create_node_execution(&inst.id, "step1", None).unwrap();
        mgr.create_node_execution(&inst.id, "step2", None).unwrap();

        let execs = mgr.list_node_executions(&inst.id).unwrap();
        assert_eq!(execs.len(), 2);
    }

    #[test]
    fn test_complete_instance() {
        let (_, mgr) = setup();
        let inst = mgr.create_instance("wf1", 1, None).unwrap();

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
