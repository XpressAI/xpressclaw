use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::state::AgentStatus;
use crate::db::Database;
use crate::error::{Error, Result};

/// Agent record combining YAML config identity with DB runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub backend: String,
    /// Runtime state from DB
    pub status: String,
    pub container_id: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub error_message: Option<String>,
}

/// Manages agent runtime state in the database.
///
/// Agent configuration (role, model, tools, llm, etc.) lives in the YAML
/// config file and is accessed via `AppState::config()`. This registry only
/// tracks runtime state: whether an agent is running, its container ID, and
/// timestamps.
pub struct AgentRegistry {
    db: Arc<Database>,
}

impl AgentRegistry {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Ensure an agent exists in the runtime state table.
    /// Called on startup to sync YAML agents into the DB.
    /// Does NOT overwrite status if the agent already exists.
    pub fn ensure(&self, name: &str, backend: &str) -> Result<AgentRecord> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO agents (id, name, backend, config, status)
                 VALUES (?1, ?2, ?3, '{}', 'stopped')",
                rusqlite::params![name, name, backend],
            )
        })?;
        self.get(name)
    }

    pub fn get(&self, agent_id: &str) -> Result<AgentRecord> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, backend, status, container_id, created_at,
                        started_at, stopped_at, error_message
                 FROM agents WHERE id = ?1",
            )?;
            let record = stmt.query_row([agent_id], |row| {
                Ok(AgentRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    backend: row.get(2)?,
                    status: row.get(3)?,
                    container_id: row.get(4)?,
                    created_at: row.get(5)?,
                    started_at: row.get(6)?,
                    stopped_at: row.get(7)?,
                    error_message: row.get(8)?,
                })
            });
            match record {
                Ok(r) => Ok(r),
                Err(_) => Err(Error::AgentNotFound {
                    name: agent_id.to_string(),
                }),
            }
        })
    }

    pub fn list(&self) -> Result<Vec<AgentRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, backend, status, container_id, created_at,
                        started_at, stopped_at, error_message
                 FROM agents ORDER BY name",
            )?;
            let records = stmt
                .query_map([], |row| {
                    Ok(AgentRecord {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        backend: row.get(2)?,
                        status: row.get(3)?,
                        container_id: row.get(4)?,
                        created_at: row.get(5)?,
                        started_at: row.get(6)?,
                        stopped_at: row.get(7)?,
                        error_message: row.get(8)?,
                    })
                })
                .map_err(|e| Error::Database(e.to_string()))?
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

    /// Reset all agent runtime state on startup.
    ///
    /// On fresh start no containers are running — any "running" status in the
    /// DB is stale from a previous session that crashed or was killed. Reset
    /// everything so the UI doesn't lie about what's actually happening.
    pub fn reset_all_on_startup(&self) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET status = 'stopped', container_id = NULL, error_message = NULL",
                [],
            )
        })?;
        debug!("reset all agent statuses to stopped on startup");
        Ok(())
    }

    /// Remove agents from DB that are no longer in the YAML config.
    pub fn remove_stale(&self, valid_names: &[&str]) -> Result<()> {
        let existing = self.list()?;
        for agent in existing {
            if !valid_names.contains(&agent.name.as_str()) {
                debug!(name = agent.name, "removing stale agent from DB");
                self.delete(&agent.id)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_and_get() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        let record = registry.ensure("atlas", "generic").unwrap();
        assert_eq!(record.name, "atlas");
        assert_eq!(record.backend, "generic");
        assert_eq!(record.status, "stopped");

        // Ensure again doesn't overwrite status
        let fetched = registry.ensure("atlas", "generic").unwrap();
        assert_eq!(fetched.id, record.id);
    }

    #[test]
    fn test_update_status() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry.ensure("atlas", "generic").unwrap();

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

        registry.ensure("atlas", "generic").unwrap();
        registry.ensure("hermes", "claude-sdk").unwrap();

        let agents = registry.list().unwrap();
        assert_eq!(agents.len(), 2);
    }

    #[test]
    fn test_delete_agent() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry.ensure("atlas", "generic").unwrap();
        registry.delete("atlas").unwrap();
        assert!(registry.get("atlas").is_err());
    }

    #[test]
    fn test_error_status() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry.ensure("atlas", "generic").unwrap();

        let updated = registry
            .update_status("atlas", &AgentStatus::Error("OOM killed".into()), None)
            .unwrap();
        assert_eq!(updated.status, "error");
        assert_eq!(updated.error_message.as_deref(), Some("OOM killed"));
    }

    #[test]
    fn test_remove_stale() {
        let db = Arc::new(Database::open_memory().unwrap());
        let registry = AgentRegistry::new(db);

        registry.ensure("atlas", "generic").unwrap();
        registry.ensure("hermes", "claude-sdk").unwrap();
        registry.ensure("old_agent", "generic").unwrap();

        registry.remove_stale(&["atlas", "hermes"]).unwrap();

        let agents = registry.list().unwrap();
        assert_eq!(agents.len(), 2);
        assert!(registry.get("old_agent").is_err());
    }
}
