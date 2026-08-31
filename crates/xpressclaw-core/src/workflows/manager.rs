use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};

use super::definition::WorkflowDefinition;

/// A workflow definition record as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub yaml_content: String,
    pub enabled: bool,
    /// Whether this workflow is attached once to every ordinary Agent task.
    /// This is local execution policy and is intentionally stored outside the
    /// portable YAML definition.
    #[serde(default)]
    pub default_for_tasks: bool,
    pub version: u32,
    pub created_at: String,
    pub updated_at: String,
    pub last_triggered_at: Option<String>,
    pub trigger_count: i64,
    pub trigger_error: Option<String>,
}

/// Request to create a new workflow.
pub struct CreateWorkflow {
    pub name: String,
    pub description: Option<String>,
    pub yaml_content: String,
}

/// Manages CRUD operations for workflow definitions.
pub struct WorkflowManager {
    db: Arc<Database>,
}

impl WorkflowManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Create a new workflow. Parses and validates the YAML before saving.
    pub fn create(&self, req: &CreateWorkflow) -> Result<WorkflowRecord> {
        let def = WorkflowDefinition::parse(&req.yaml_content)?;
        def.validate()?;

        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflows (id, name, description, yaml_content, enabled, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6)",
                rusqlite::params![id, req.name, req.description, req.yaml_content, def.version, now],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        self.get(&id)
    }

    /// Get a workflow by ID.
    pub fn get(&self, id: &str) -> Result<WorkflowRecord> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflows WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_workflow(row)))
                .map_err(|_| Error::WorkflowNotFound { id: id.to_string() })
        })
    }

    /// List all workflows.
    pub fn list(&self) -> Result<Vec<WorkflowRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM workflows ORDER BY created_at DESC")
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([], |row| Ok(row_to_workflow(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// List enabled workflows that should run once for each ordinary task.
    pub fn list_default_for_tasks(&self) -> Result<Vec<WorkflowRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM workflows
                     WHERE enabled = 1 AND default_for_tasks = 1
                     ORDER BY created_at ASC, id ASC",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([], |row| Ok(row_to_workflow(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|record| record.ok())
                .collect();

            Ok(records)
        })
    }

    /// Update a workflow's YAML content. Re-parses and validates before saving.
    pub fn update(&self, id: &str, yaml_content: &str) -> Result<WorkflowRecord> {
        let def = WorkflowDefinition::parse(yaml_content)?;
        def.validate()?;
        let previous = self.get(id)?;
        if previous.default_for_tasks {
            def.validate_default_task_trigger()?;
            if def.uses_connector_automation() {
                return Err(Error::Workflow(
                    "default task workflows cannot use connector triggers or notification sinks"
                        .into(),
                ));
            }
        }
        let previous_schedule = WorkflowDefinition::parse(&previous.yaml_content)?.schedule;
        let schedule_changed = previous_schedule != def.schedule;

        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflows
                 SET yaml_content = ?1,
                     version = ?2,
                     name = ?3,
                     description = ?4,
                     updated_at = ?5,
                     last_triggered_at = CASE WHEN ?6 THEN NULL ELSE last_triggered_at END,
                     trigger_error = CASE WHEN ?6 THEN NULL ELSE trigger_error END
                 WHERE id = ?7",
                rusqlite::params![
                    yaml_content,
                    def.version,
                    def.name,
                    def.description,
                    now,
                    schedule_changed,
                    id
                ],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::WorkflowNotFound { id: id.to_string() });
        }

        self.get(id)
    }

    /// Delete a workflow.
    pub fn delete(&self, id: &str) -> Result<()> {
        let affected = self.db.with_conn(|conn| {
            conn.execute("DELETE FROM workflows WHERE id = ?1", [id])
                .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::WorkflowNotFound { id: id.to_string() });
        }
        Ok(())
    }

    /// Set the enabled flag on a workflow.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<WorkflowRecord> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflows SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![enabled as i32, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::WorkflowNotFound { id: id.to_string() });
        }

        self.get(id)
    }

    /// Enable or disable automatic attachment to ordinary Agent tasks.
    pub fn set_default_for_tasks(
        &self,
        id: &str,
        default_for_tasks: bool,
    ) -> Result<WorkflowRecord> {
        if default_for_tasks {
            let record = self.get(id)?;
            let definition = WorkflowDefinition::parse(&record.yaml_content)?;
            definition.validate_default_task_trigger()?;
            if definition.uses_connector_automation() {
                return Err(Error::Workflow(
                    "default task workflows cannot use connector triggers or notification sinks"
                        .into(),
                ));
            }
        }

        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflows
                 SET default_for_tasks = ?1,
                     enabled = CASE WHEN ?1 = 1 THEN 1 ELSE enabled END,
                     updated_at = ?2
                 WHERE id = ?3",
                rusqlite::params![default_for_tasks as i32, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        if affected == 0 {
            return Err(Error::WorkflowNotFound { id: id.to_string() });
        }
        self.get(id)
    }

    /// Atomically claim a due scheduled run. Comparing the previously observed
    /// timestamp prevents duplicate starts if multiple runners overlap.
    pub fn claim_scheduled_run(
        &self,
        id: &str,
        previous_triggered_at: Option<&str>,
        triggered_at: &str,
    ) -> Result<bool> {
        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflows
                 SET last_triggered_at = ?1,
                     trigger_count = trigger_count + 1,
                     trigger_error = NULL
                 WHERE id = ?2
                   AND enabled = 1
                   AND ((last_triggered_at IS NULL AND ?3 IS NULL) OR last_triggered_at = ?3)",
                rusqlite::params![triggered_at, id, previous_triggered_at],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        Ok(affected == 1)
    }

    /// Record why the latest automatic trigger could not start an instance.
    pub fn record_trigger_error(&self, id: &str, message: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflows SET trigger_error = ?1 WHERE id = ?2",
                rusqlite::params![message, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        Ok(())
    }
}

fn row_to_workflow(row: &rusqlite::Row) -> WorkflowRecord {
    WorkflowRecord {
        id: row.get("id").unwrap_or_default(),
        name: row.get("name").unwrap_or_default(),
        description: row.get("description").unwrap_or_default(),
        yaml_content: row.get("yaml_content").unwrap_or_default(),
        enabled: row.get::<_, i32>("enabled").unwrap_or(1) != 0,
        default_for_tasks: row.get::<_, i32>("default_for_tasks").unwrap_or(0) != 0,
        version: row.get::<_, u32>("version").unwrap_or(1),
        created_at: row.get("created_at").unwrap_or_default(),
        updated_at: row.get("updated_at").unwrap_or_default(),
        last_triggered_at: row.get("last_triggered_at").unwrap_or_default(),
        trigger_count: row.get("trigger_count").unwrap_or(0),
        trigger_error: row.get("trigger_error").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, WorkflowManager) {
        let db = Arc::new(Database::open_memory().unwrap());
        let mgr = WorkflowManager::new(db.clone());
        (db, mgr)
    }

    const VALID_YAML: &str = r#"
name: test-workflow
description: A test workflow
version: 1
flows:
  main:
    steps:
      - id: step1
        label: "First Step"
        agent: atlas
        prompt: "Do something"
      - id: step2
        label: "Second Step"
        agent: atlas
        prompt: "Do another thing"
"#;

    #[test]
    fn test_create_and_get() {
        let (_, mgr) = setup();
        let record = mgr
            .create(&CreateWorkflow {
                name: "test-workflow".into(),
                description: Some("A test workflow".into()),
                yaml_content: VALID_YAML.into(),
            })
            .unwrap();

        assert_eq!(record.name, "test-workflow");
        assert_eq!(record.version, 1);
        assert!(record.enabled);

        let fetched = mgr.get(&record.id).unwrap();
        assert_eq!(fetched.id, record.id);
        assert_eq!(fetched.name, "test-workflow");
    }

    #[test]
    fn test_create_invalid_yaml() {
        let (_, mgr) = setup();
        let result = mgr.create(&CreateWorkflow {
            name: "bad".into(),
            description: None,
            yaml_content: "not: [valid: yaml: workflow".into(),
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_create_invalid_definition() {
        let (_, mgr) = setup();
        let yaml = r#"
name: empty
flows: {}
"#;
        let result = mgr.create(&CreateWorkflow {
            name: "empty".into(),
            description: None,
            yaml_content: yaml.into(),
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_list() {
        let (_, mgr) = setup();
        mgr.create(&CreateWorkflow {
            name: "wf1".into(),
            description: None,
            yaml_content: VALID_YAML.into(),
        })
        .unwrap();

        let yaml2 = VALID_YAML.replace("test-workflow", "second-workflow");
        mgr.create(&CreateWorkflow {
            name: "wf2".into(),
            description: None,
            yaml_content: yaml2,
        })
        .unwrap();

        let all = mgr.list().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_update() {
        let (_, mgr) = setup();
        let record = mgr
            .create(&CreateWorkflow {
                name: "test-workflow".into(),
                description: None,
                yaml_content: VALID_YAML.into(),
            })
            .unwrap();

        let updated_yaml = VALID_YAML.replace("test-workflow", "updated-workflow");
        let updated = mgr.update(&record.id, &updated_yaml).unwrap();
        assert_eq!(updated.name, "updated-workflow");
    }

    #[test]
    fn test_update_not_found() {
        let (_, mgr) = setup();
        let result = mgr.update("nonexistent", VALID_YAML);
        assert!(result.is_err());
    }

    #[test]
    fn test_delete() {
        let (_, mgr) = setup();
        let record = mgr
            .create(&CreateWorkflow {
                name: "test-workflow".into(),
                description: None,
                yaml_content: VALID_YAML.into(),
            })
            .unwrap();

        mgr.delete(&record.id).unwrap();
        assert!(mgr.get(&record.id).is_err());
    }

    #[test]
    fn test_delete_not_found() {
        let (_, mgr) = setup();
        assert!(mgr.delete("nonexistent").is_err());
    }

    #[test]
    fn test_set_enabled() {
        let (_, mgr) = setup();
        let record = mgr
            .create(&CreateWorkflow {
                name: "test-workflow".into(),
                description: None,
                yaml_content: VALID_YAML.into(),
            })
            .unwrap();

        let disabled = mgr.set_enabled(&record.id, false).unwrap();
        assert!(!disabled.enabled);

        let enabled = mgr.set_enabled(&record.id, true).unwrap();
        assert!(enabled.enabled);
    }

    #[test]
    fn default_task_workflows_are_explicit_and_enabled() {
        let (_, mgr) = setup();
        let record = mgr
            .create(&CreateWorkflow {
                name: "default-policy".into(),
                description: None,
                yaml_content: r#"
name: default-policy
flows:
  main:
    steps:
      - id: final_check
        type: continue
        prompt: Check the finished task.
"#
                .into(),
            })
            .unwrap();
        assert!(!record.default_for_tasks);
        assert!(mgr.list_default_for_tasks().unwrap().is_empty());

        mgr.set_enabled(&record.id, false).unwrap();
        let default = mgr.set_default_for_tasks(&record.id, true).unwrap();
        assert!(default.default_for_tasks);
        assert!(default.enabled);
        assert_eq!(mgr.list_default_for_tasks().unwrap().len(), 1);

        let manual = mgr.set_default_for_tasks(&record.id, false).unwrap();
        assert!(!manual.default_for_tasks);
        assert!(mgr.list_default_for_tasks().unwrap().is_empty());
    }

    #[test]
    fn scheduled_run_claims_are_atomic_and_schedule_edits_reset_the_cursor() {
        let (_, mgr) = setup();
        let scheduled_yaml =
            VALID_YAML.replace("flows:", "schedule:\n  cron: \"0 9 * * 1\"\n\nflows:");
        let record = mgr
            .create(&CreateWorkflow {
                name: "test-workflow".into(),
                description: None,
                yaml_content: scheduled_yaml.clone(),
            })
            .unwrap();

        assert!(mgr
            .claim_scheduled_run(&record.id, None, "2026-08-02 09:00:00")
            .unwrap());
        assert!(!mgr
            .claim_scheduled_run(&record.id, None, "2026-08-02 09:00:00")
            .unwrap());
        let claimed = mgr.get(&record.id).unwrap();
        assert_eq!(claimed.trigger_count, 1);

        let renamed = scheduled_yaml.replace("A test workflow", "An updated workflow");
        let unchanged_schedule = mgr.update(&record.id, &renamed).unwrap();
        assert_eq!(
            unchanged_schedule.last_triggered_at.as_deref(),
            Some("2026-08-02 09:00:00")
        );

        let changed_cron = renamed.replace("0 9 * * 1", "0 10 * * 1");
        let changed_schedule = mgr.update(&record.id, &changed_cron).unwrap();
        assert!(changed_schedule.last_triggered_at.is_none());
        assert_eq!(changed_schedule.trigger_count, 1);
    }

    #[test]
    fn test_get_not_found() {
        let (_, mgr) = setup();
        assert!(matches!(
            mgr.get("nonexistent"),
            Err(Error::WorkflowNotFound { .. })
        ));
    }
}
