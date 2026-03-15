use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};

/// An input parameter for an SOP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopInput {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
    pub default: Option<String>,
}

/// An output definition for an SOP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopOutput {
    pub name: String,
    pub description: String,
}

/// A step in an SOP.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopStep {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub optional: bool,
}

/// The YAML body of an SOP, parsed from the `content` column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopContent {
    pub summary: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub inputs: Vec<SopInput>,
    #[serde(default)]
    pub outputs: Vec<SopOutput>,
    #[serde(default)]
    pub steps: Vec<SopStep>,
}

/// An SOP stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sop {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Raw YAML content.
    pub content: String,
    pub triggers: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub created_by: Option<String>,
    pub version: i32,
    /// Parsed content (populated on read).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<SopContent>,
}

/// Request to create a new SOP.
#[derive(Debug, Deserialize)]
pub struct CreateSop {
    pub name: String,
    pub description: Option<String>,
    pub content: String,
    pub triggers: Option<String>,
    pub created_by: Option<String>,
}

/// Request to update an SOP.
#[derive(Debug, Deserialize)]
pub struct UpdateSop {
    pub description: Option<String>,
    pub content: Option<String>,
    pub triggers: Option<String>,
}

/// Manages Standard Operating Procedures stored in the database.
///
/// SOPs define repeatable workflows with inputs, steps, outputs, and tool
/// requirements. They are stored as YAML in the `content` column and parsed
/// on read.
pub struct SopManager {
    db: Arc<Database>,
}

impl SopManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Create a new SOP.
    pub fn create(&self, req: &CreateSop) -> Result<Sop> {
        // Validate YAML content
        let _parsed: SopContent = serde_yaml::from_str(&req.content)
            .map_err(|e| Error::Sop(format!("invalid SOP content YAML: {e}")))?;

        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sops (id, name, description, content, triggers, created_at, updated_at, created_by, version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)",
                rusqlite::params![
                    id,
                    req.name,
                    req.description,
                    req.content,
                    req.triggers,
                    now,
                    now,
                    req.created_by,
                ],
            )
            .map_err(|e| {
                if e.to_string().contains("UNIQUE") {
                    Error::Sop(format!("SOP with name '{}' already exists", req.name))
                } else {
                    Error::Database(e.to_string())
                }
            })
        })?;

        self.get_by_id(&id)
    }

    /// Get an SOP by ID.
    pub fn get_by_id(&self, id: &str) -> Result<Sop> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM sops WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_sop(row)))
                .map_err(|_| Error::SopNotFound {
                    name: id.to_string(),
                })
        })
    }

    /// Get an SOP by name.
    pub fn get_by_name(&self, name: &str) -> Result<Sop> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM sops WHERE name = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([name], |row| Ok(row_to_sop(row)))
                .map_err(|_| Error::SopNotFound {
                    name: name.to_string(),
                })
        })
    }

    /// List all SOPs.
    pub fn list(&self) -> Result<Vec<Sop>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM sops ORDER BY name ASC")
                .map_err(|e| Error::Database(e.to_string()))?;

            let sops = stmt
                .query_map([], |row| Ok(row_to_sop(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(sops)
        })
    }

    /// Update an SOP by name. Bumps the version number.
    pub fn update(&self, name: &str, req: &UpdateSop) -> Result<Sop> {
        // If content is being updated, validate it
        if let Some(ref content) = req.content {
            let _parsed: SopContent = serde_yaml::from_str(content)
                .map_err(|e| Error::Sop(format!("invalid SOP content YAML: {e}")))?;
        }

        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        // Verify it exists
        let existing = self.get_by_name(name)?;

        self.db.with_conn(|conn| {
            if let Some(ref desc) = req.description {
                conn.execute(
                    "UPDATE sops SET description = ?1, updated_at = ?2 WHERE name = ?3",
                    rusqlite::params![desc, now, name],
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            }
            if let Some(ref content) = req.content {
                conn.execute(
                    "UPDATE sops SET content = ?1, updated_at = ?2, version = version + 1 WHERE name = ?3",
                    rusqlite::params![content, now, name],
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            }
            if let Some(ref triggers) = req.triggers {
                conn.execute(
                    "UPDATE sops SET triggers = ?1, updated_at = ?2 WHERE name = ?3",
                    rusqlite::params![triggers, now, name],
                )
                .map_err(|e| Error::Database(e.to_string()))?;
            }
            Ok::<_, Error>(())
        })?;

        self.get_by_id(&existing.id)
    }

    /// Delete an SOP by name.
    pub fn delete(&self, name: &str) -> Result<()> {
        let affected = self.db.with_conn(|conn| {
            conn.execute("DELETE FROM sops WHERE name = ?1", [name])
                .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::SopNotFound {
                name: name.to_string(),
            });
        }
        Ok(())
    }

    /// Build a task description from an SOP with provided input values.
    pub fn format_task_description(
        &self,
        sop: &Sop,
        inputs: &std::collections::HashMap<String, String>,
    ) -> String {
        let fallback = serde_yaml::from_str::<SopContent>(&sop.content).ok();
        let parsed = sop.parsed.as_ref().or(fallback.as_ref());

        let mut desc = format!("## SOP: {}\n\n", sop.name);

        if let Some(ref d) = sop.description {
            desc.push_str(&format!("{d}\n\n"));
        }

        if !inputs.is_empty() {
            desc.push_str("### Inputs\n\n");
            for (key, value) in inputs {
                desc.push_str(&format!("- **{key}**: {value}\n"));
            }
            desc.push('\n');
        }

        if let Some(parsed) = parsed {
            if !parsed.steps.is_empty() {
                desc.push_str("### Steps\n\n");
                for (i, step) in parsed.steps.iter().enumerate() {
                    let prefix = if step.optional { "(optional) " } else { "" };
                    desc.push_str(&format!(
                        "{}. {}{}: {}\n",
                        i + 1,
                        prefix,
                        step.name,
                        step.description
                    ));
                }
                desc.push('\n');
            }

            if !parsed.outputs.is_empty() {
                desc.push_str("### Expected Outputs\n\n");
                for output in &parsed.outputs {
                    desc.push_str(&format!("- **{}**: {}\n", output.name, output.description));
                }
            }
        }

        desc
    }
}

fn row_to_sop(row: &rusqlite::Row) -> Sop {
    let content: String = row.get("content").unwrap_or_default();
    let parsed = serde_yaml::from_str::<SopContent>(&content).ok();

    Sop {
        id: row.get("id").unwrap_or_default(),
        name: row.get("name").unwrap_or_default(),
        description: row.get("description").unwrap_or_default(),
        content,
        triggers: row.get("triggers").unwrap_or_default(),
        created_at: row.get("created_at").unwrap_or_default(),
        updated_at: row.get("updated_at").unwrap_or_default(),
        created_by: row.get("created_by").unwrap_or_default(),
        version: row.get("version").unwrap_or(1),
        parsed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, SopManager) {
        let db = Arc::new(Database::open_memory().unwrap());
        let mgr = SopManager::new(db.clone());
        (db, mgr)
    }

    const TEST_SOP_CONTENT: &str = r#"
summary: Deploy a service to production
tools:
  - shell
  - git
inputs:
  - name: service_name
    description: Name of the service to deploy
    required: true
  - name: version
    description: Version tag to deploy
    required: true
    default: latest
outputs:
  - name: deploy_url
    description: URL of the deployed service
steps:
  - name: Pull latest
    description: Pull the latest code from git
    tools: [git]
  - name: Run tests
    description: Execute the test suite
    tools: [shell]
  - name: Build
    description: Build the Docker image
    tools: [shell]
  - name: Deploy
    description: Push and deploy to production
    tools: [shell]
  - name: Verify
    description: Check health endpoint
    tools: [shell]
    optional: true
"#;

    fn create_sop(mgr: &SopManager) -> Sop {
        mgr.create(&CreateSop {
            name: "deploy-service".to_string(),
            description: Some("Deploy a service to production".to_string()),
            content: TEST_SOP_CONTENT.to_string(),
            triggers: None,
            created_by: Some("user".to_string()),
        })
        .unwrap()
    }

    #[test]
    fn test_create_and_get() {
        let (_, mgr) = setup();
        let sop = create_sop(&mgr);

        assert_eq!(sop.name, "deploy-service");
        assert_eq!(sop.version, 1);
        assert!(sop.parsed.is_some());

        let parsed = sop.parsed.unwrap();
        assert_eq!(parsed.steps.len(), 5);
        assert_eq!(parsed.inputs.len(), 2);
        assert_eq!(parsed.outputs.len(), 1);
        assert_eq!(parsed.tools, vec!["shell", "git"]);

        // Verify optional step
        assert!(!parsed.steps[0].optional);
        assert!(parsed.steps[4].optional);
    }

    #[test]
    fn test_get_by_name() {
        let (_, mgr) = setup();
        create_sop(&mgr);

        let sop = mgr.get_by_name("deploy-service").unwrap();
        assert_eq!(sop.name, "deploy-service");
    }

    #[test]
    fn test_get_not_found() {
        let (_, mgr) = setup();
        let result = mgr.get_by_name("nonexistent");
        assert!(matches!(result, Err(Error::SopNotFound { .. })));
    }

    #[test]
    fn test_list() {
        let (_, mgr) = setup();
        create_sop(&mgr);

        mgr.create(&CreateSop {
            name: "code-review".to_string(),
            description: Some("Review a PR".to_string()),
            content:
                "summary: Review code\nsteps:\n  - name: Review\n    description: Check the code\n"
                    .to_string(),
            triggers: None,
            created_by: None,
        })
        .unwrap();

        let all = mgr.list().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_update_content_bumps_version() {
        let (_, mgr) = setup();
        create_sop(&mgr);

        let updated = mgr
            .update(
                "deploy-service",
                &UpdateSop {
                    description: None,
                    content: Some("summary: Updated deploy\nsteps:\n  - name: Deploy\n    description: Just deploy\n".to_string()),
                    triggers: None,
                },
            )
            .unwrap();

        assert_eq!(updated.version, 2);
        let parsed = updated.parsed.unwrap();
        assert_eq!(parsed.steps.len(), 1);
    }

    #[test]
    fn test_update_description_only() {
        let (_, mgr) = setup();
        create_sop(&mgr);

        let updated = mgr
            .update(
                "deploy-service",
                &UpdateSop {
                    description: Some("New description".to_string()),
                    content: None,
                    triggers: None,
                },
            )
            .unwrap();

        assert_eq!(updated.description.as_deref(), Some("New description"));
        assert_eq!(updated.version, 1); // version not bumped for description-only change
    }

    #[test]
    fn test_delete() {
        let (_, mgr) = setup();
        create_sop(&mgr);

        mgr.delete("deploy-service").unwrap();
        assert!(mgr.get_by_name("deploy-service").is_err());
    }

    #[test]
    fn test_delete_not_found() {
        let (_, mgr) = setup();
        let result = mgr.delete("nonexistent");
        assert!(matches!(result, Err(Error::SopNotFound { .. })));
    }

    #[test]
    fn test_duplicate_name_rejected() {
        let (_, mgr) = setup();
        create_sop(&mgr);

        let result = mgr.create(&CreateSop {
            name: "deploy-service".to_string(),
            description: None,
            content: "summary: Duplicate\nsteps: []\n".to_string(),
            triggers: None,
            created_by: None,
        });

        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_yaml_rejected() {
        let (_, mgr) = setup();
        let result = mgr.create(&CreateSop {
            name: "bad-sop".to_string(),
            description: None,
            content: "not: [valid: yaml: content".to_string(),
            triggers: None,
            created_by: None,
        });

        assert!(result.is_err());
    }

    #[test]
    fn test_format_task_description() {
        let (_, mgr) = setup();
        let sop = create_sop(&mgr);

        let mut inputs = std::collections::HashMap::new();
        inputs.insert("service_name".to_string(), "api-gateway".to_string());
        inputs.insert("version".to_string(), "v2.1.0".to_string());

        let desc = mgr.format_task_description(&sop, &inputs);

        assert!(desc.contains("## SOP: deploy-service"));
        assert!(desc.contains("**service_name**: api-gateway"));
        assert!(desc.contains("**version**: v2.1.0"));
        assert!(desc.contains("### Steps"));
        assert!(desc.contains("1. Pull latest"));
        assert!(desc.contains("5. (optional) Verify"));
        assert!(desc.contains("### Expected Outputs"));
        assert!(desc.contains("deploy_url"));
    }
}
