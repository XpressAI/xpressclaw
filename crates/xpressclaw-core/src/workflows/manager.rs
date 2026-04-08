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
    pub version: u32,
    pub created_at: String,
    pub updated_at: String,
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

    /// Update a workflow's YAML content. Re-parses and validates before saving.
    pub fn update(&self, id: &str, yaml_content: &str) -> Result<WorkflowRecord> {
        let def = WorkflowDefinition::parse(yaml_content)?;
        def.validate()?;

        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE workflows SET yaml_content = ?1, version = ?2, name = ?3, description = ?4, updated_at = ?5 WHERE id = ?6",
                rusqlite::params![yaml_content, def.version, def.name, def.description, now, id],
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
}

fn row_to_workflow(row: &rusqlite::Row) -> WorkflowRecord {
    WorkflowRecord {
        id: row.get("id").unwrap_or_default(),
        name: row.get("name").unwrap_or_default(),
        description: row.get("description").unwrap_or_default(),
        yaml_content: row.get("yaml_content").unwrap_or_default(),
        enabled: row.get::<_, i32>("enabled").unwrap_or(1) != 0,
        version: row.get::<_, u32>("version").unwrap_or(1),
        created_at: row.get("created_at").unwrap_or_default(),
        updated_at: row.get("updated_at").unwrap_or_default(),
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
    fn test_get_not_found() {
        let (_, mgr) = setup();
        assert!(matches!(
            mgr.get("nonexistent"),
            Err(Error::WorkflowNotFound { .. })
        ));
    }
}
