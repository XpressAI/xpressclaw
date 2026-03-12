use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::state::AgentStatus;
use crate::db::Database;
use crate::error::{Error, Result};

/// Persistent agent record in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub backend: String,
    pub config: serde_json::Value,
    pub status: String,
    pub container_id: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub error_message: Option<String>,
}

/// Input for registering/updating an agent.
#[derive(Debug, Deserialize)]
pub struct RegisterAgent {
    pub name: String,
    pub backend: String,
    pub config: serde_json::Value,
}

/// Manages agent registration and status in the database.
pub struct AgentRegistry {
    db: Arc<Database>,
}

impl AgentRegistry {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn register(&self, req: &RegisterAgent) -> Result<AgentRecord> {
        let config_json = req.config.to_string();

        // Use name as id for simplicity
        let id = req.name.clone();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO agents (id, name, backend, config, status) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, req.name, req.backend, config_json, "stopped"],
            )
        })?;

        self.get(&id)
    }

    pub fn get(&self, agent_id: &str) -> Result<AgentRecord> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT * FROM agents WHERE id = ?1")?;
            stmt.query_row([agent_id], |row| Ok(row_to_record(row)))
                .map_err(|_| Error::AgentNotFound {
                    name: agent_id.to_string(),
                })
        })?
    }

    pub fn list(&self) -> Result<Vec<AgentRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT * FROM agents ORDER BY name")?;
            let records = stmt
                .query_map([], |row| Ok(row_to_record(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .filter_map(|r| r.ok())
                .collect();
            Ok(records)
        })
    }

    pub fn update_status(
        &self,
        agent_id: &str,
        status: &AgentStatus,
        container_id: Option<&str>,
    ) -> Result<AgentRecord> {
        let status_str = match status {
            AgentStatus::Error(msg) => {
                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE agents SET status = 'error', error_message = ?1 WHERE id = ?2",
                        rusqlite::params![msg, agent_id],
                    )
                })?;
                "error".to_string()
            }
            _ => {
                let s = status.to_string();
                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE agents SET status = ?1, error_message = NULL WHERE id = ?2",
                        rusqlite::params![s, agent_id],
                    )
                })?;
                s
            }
        };

        if let Some(cid) = container_id {
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE agents SET container_id = ?1 WHERE id = ?2",
                    rusqlite::params![cid, agent_id],
                )
            })?;
        }

        // Set timestamps
        match status {
            AgentStatus::Running => {
                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE agents SET started_at = CURRENT_TIMESTAMP WHERE id = ?1",
                        [agent_id],
                    )
                })?;
            }
            AgentStatus::Stopped => {
                self.db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE agents SET stopped_at = CURRENT_TIMESTAMP, container_id = NULL WHERE id = ?1",
                        [agent_id],
                    )
                })?;
            }
            _ => {}
        }

        debug!(agent_id, status = status_str, "updated agent status");
        self.get(agent_id)
    }

    pub fn delete(&self, agent_id: &str) -> Result<()> {
        self.db
            .with_conn(|conn| conn.execute("DELETE FROM agents WHERE id = ?1", [agent_id]))?;
        Ok(())
    }
}

fn row_to_record(row: &rusqlite::Row) -> Result<AgentRecord> {
    let config_str: String = row.get("config")?;
    let config: serde_json::Value =
        serde_json::from_str(&config_str).unwrap_or(serde_json::Value::Object(Default::default()));

    Ok(AgentRecord {
        id: row.get("id")?,
        name: row.get("name")?,
        backend: row.get("backend")?,
        config,
        status: row.get("status")?,
        container_id: row.get("container_id")?,
        created_at: row.get("created_at")?,
        started_at: row.get("started_at")?,
        stopped_at: row.get("stopped_at")?,
        error_message: row.get("error_message")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_get() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        let record = registry
            .register(&RegisterAgent {
                name: "atlas".to_string(),
                backend: "generic".to_string(),
                config: serde_json::json!({"role": "You are a helpful assistant"}),
            })
            .unwrap();

        assert_eq!(record.name, "atlas");
        assert_eq!(record.backend, "generic");
        assert_eq!(record.status, "stopped");

        let fetched = registry.get("atlas").unwrap();
        assert_eq!(fetched.id, record.id);
    }

    #[test]
    fn test_update_status() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry
            .register(&RegisterAgent {
                name: "atlas".to_string(),
                backend: "generic".to_string(),
                config: serde_json::json!({}),
            })
            .unwrap();

        let updated = registry
            .update_status("atlas", &AgentStatus::Running, Some("abc123"))
            .unwrap();
        assert_eq!(updated.status, "running");
        assert_eq!(updated.container_id.as_deref(), Some("abc123"));

        let stopped = registry
            .update_status("atlas", &AgentStatus::Stopped, None)
            .unwrap();
        assert_eq!(stopped.status, "stopped");
        assert!(stopped.container_id.is_none());
    }

    #[test]
    fn test_list_agents() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry
            .register(&RegisterAgent {
                name: "atlas".into(),
                backend: "generic".into(),
                config: serde_json::json!({}),
            })
            .unwrap();
        registry
            .register(&RegisterAgent {
                name: "hermes".into(),
                backend: "claude-sdk".into(),
                config: serde_json::json!({}),
            })
            .unwrap();

        let agents = registry.list().unwrap();
        assert_eq!(agents.len(), 2);
    }

    #[test]
    fn test_delete_agent() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry
            .register(&RegisterAgent {
                name: "atlas".into(),
                backend: "generic".into(),
                config: serde_json::json!({}),
            })
            .unwrap();

        registry.delete("atlas").unwrap();
        assert!(registry.get("atlas").is_err());
    }

    #[test]
    fn test_error_status() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry
            .register(&RegisterAgent {
                name: "atlas".into(),
                backend: "generic".into(),
                config: serde_json::json!({}),
            })
            .unwrap();

        let updated = registry
            .update_status("atlas", &AgentStatus::Error("OOM killed".into()), None)
            .unwrap();
        assert_eq!(updated.status, "error");
        assert_eq!(updated.error_message.as_deref(), Some("OOM killed"));
    }
}
