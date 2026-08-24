use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::state::AgentStatus;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::projects::ensure_project_accepts_work;

/// Agent record combining YAML config identity with DB runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub id: String,
    pub name: String,
    pub backend: String,
    /// Project that owns this Agent's conversations, tasks, and retained
    /// workspace. Existing installations receive one project per Agent.
    pub project_id: Option<String>,
    /// Old status column — will be removed once all callers migrate.
    pub status: String,
    pub container_id: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub stopped_at: Option<String>,
    pub error_message: Option<String>,
    /// Desired state: what the user wants ('running' or 'stopped').
    pub desired_status: String,
    /// How many times the reconciler has tried to start this agent
    /// since it last ran stably. Used for exponential backoff.
    pub restart_count: i32,
    /// When the reconciler last attempted to start this agent.
    pub last_attempt_at: Option<String>,
    /// How many consecutive idle-task cycles have run (XCLAW-47).
    pub idle_count: i32,
    /// When the last idle check occurred.
    pub last_idle_check: Option<String>,
}

/// Manages agent runtime state in the database.
///
/// Session runtime configuration (runner, workspace, mounts, etc.) lives in the YAML
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
                "INSERT OR IGNORE INTO projects (id, name, description)
                 SELECT ?1, ?1, 'Created with this Agent'
                 WHERE NOT EXISTS (
                     SELECT 1 FROM agents WHERE id = ?1 AND project_id IS NOT NULL
                 )",
                [name],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO agents (id, name, backend, config, status, project_id)
                 VALUES (?1, ?2, ?3, '{}', 'stopped', ?1)",
                rusqlite::params![name, name, backend],
            )?;
            conn.execute(
                "UPDATE agents SET project_id = COALESCE(project_id, ?1) WHERE id = ?1",
                [name],
            )
        })?;
        self.get(name)
    }

    /// Create a new Agent directly inside an existing Project.
    ///
    /// Reserving the SQLite writer before validating the Project keeps Project
    /// deletion from committing between validation and Agent attachment. This
    /// deliberately uses `INSERT`, not `INSERT OR IGNORE`: callers creating a
    /// new configured Agent must not silently adopt an unrelated stale row.
    pub fn create_in_project(
        &self,
        name: &str,
        backend: &str,
        project_id: &str,
    ) -> Result<AgentRecord> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_project_accepts_work(&transaction, project_id)?;
            transaction.execute(
                "INSERT INTO agents (id, name, backend, config, status, project_id)
                 VALUES (?1, ?1, ?2, '{}', 'stopped', ?3)",
                rusqlite::params![name, backend, project_id],
            )?;
            transaction.execute(
                "UPDATE projects SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                [project_id],
            )?;
            transaction.commit()?;
            Ok::<(), Error>(())
        })?;
        self.get(name)
    }

    pub fn get(&self, agent_id: &str) -> Result<AgentRecord> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, backend, project_id, status, container_id, created_at,
                        started_at, stopped_at, error_message,
                        desired_status, restart_count, last_attempt_at,
                        idle_count, last_idle_check
                 FROM agents WHERE id = ?1",
            )?;
            let record = stmt.query_row([agent_id], |row| {
                Ok(AgentRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    backend: row.get(2)?,
                    project_id: row.get(3)?,
                    status: row.get(4)?,
                    container_id: row.get(5)?,
                    created_at: row.get(6)?,
                    started_at: row.get(7)?,
                    stopped_at: row.get(8)?,
                    error_message: row.get(9)?,
                    desired_status: row.get(10)?,
                    restart_count: row.get(11)?,
                    last_attempt_at: row.get(12)?,
                    idle_count: row.get(13)?,
                    last_idle_check: row.get(14)?,
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
                "SELECT id, name, backend, project_id, status, container_id, created_at,
                        started_at, stopped_at, error_message,
                        desired_status, restart_count, last_attempt_at,
                        idle_count, last_idle_check
                 FROM agents ORDER BY name",
            )?;
            let records = stmt
                .query_map([], |row| {
                    Ok(AgentRecord {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        backend: row.get(2)?,
                        project_id: row.get(3)?,
                        status: row.get(4)?,
                        container_id: row.get(5)?,
                        created_at: row.get(6)?,
                        started_at: row.get(7)?,
                        stopped_at: row.get(8)?,
                        error_message: row.get(9)?,
                        desired_status: row.get(10)?,
                        restart_count: row.get(11)?,
                        last_attempt_at: row.get(12)?,
                        idle_count: row.get(13)?,
                        last_idle_check: row.get(14)?,
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
        self.delete_with_running_conversation_turns(agent_id, |_| {})
    }

    /// Make every live Conversation turn unpublishable and notify the caller
    /// before deleting the Agent. The callback runs while the write
    /// transaction is held, after turn cancellation but before cascading
    /// deletion, so the server can interrupt retained ACP processes without a
    /// late response racing Agent deletion.
    pub fn delete_with_running_conversation_turns<F>(
        &self,
        agent_id: &str,
        mut before_delete: F,
    ) -> Result<()>
    where
        F: FnMut(&str),
    {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let mut statement = transaction.prepare(
                "SELECT id FROM conversation_turns
                 WHERE agent_id = ?1 AND status = 'running'",
            )?;
            let running_turns = statement
                .query_map([agent_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            transaction.execute(
                "UPDATE conversation_turns
                 SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                     error_message = 'Agent deleted'
                 WHERE agent_id = ?1 AND status IN ('queued', 'running')",
                [agent_id],
            )?;
            for turn_id in &running_turns {
                before_delete(turn_id);
            }
            // Participant identities are polymorphic, so the original table
            // cannot express an Agent foreign key. Remove those memberships
            // and schedules explicitly before the Agent-owned session/turn
            // rows cascade. Reserving the writer first prevents a wake-up from
            // being created after cleanup but before the Agent disappears.
            transaction.execute(
                "DELETE FROM conversation_participants
                 WHERE participant_type = 'agent' AND participant_id = ?1",
                [agent_id],
            )?;
            transaction.execute("DELETE FROM schedules WHERE agent_id = ?1", [agent_id])?;
            transaction.execute("DELETE FROM agents WHERE id = ?1", [agent_id])?;
            transaction.commit()
        })?;
        Ok(())
    }

    // -- Desired-state methods (ADR-018) --

    /// Set the desired status for an agent. Resets restart backoff.
    pub fn set_desired_status(
        &self,
        agent_id: &str,
        desired: &crate::agents::state::DesiredStatus,
    ) -> Result<()> {
        let s = desired.to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET desired_status = ?1, restart_count = 0,
                 last_attempt_at = NULL, error_message = NULL WHERE id = ?2",
                rusqlite::params![s, agent_id],
            )
        })?;
        debug!(agent_id, desired_status = s, "set desired status");
        Ok(())
    }

    /// Record a reconciliation attempt (success or failure).
    pub fn record_attempt(&self, agent_id: &str, error: Option<&str>) -> Result<()> {
        if let Some(err) = error {
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE agents SET restart_count = restart_count + 1,
                     last_attempt_at = CURRENT_TIMESTAMP,
                     error_message = ?1 WHERE id = ?2",
                    rusqlite::params![err, agent_id],
                )
            })?;
        } else {
            self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE agents SET last_attempt_at = CURRENT_TIMESTAMP,
                     error_message = NULL WHERE id = ?1",
                    [agent_id],
                )
            })?;
        }
        Ok(())
    }

    /// Reset restart count (agent has been running stably).
    pub fn reset_restart_count(&self, agent_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET restart_count = 0, error_message = NULL WHERE id = ?1",
                [agent_id],
            )
        })?;
        Ok(())
    }

    /// Clear a recovered runtime error without changing the logical session status.
    pub fn clear_error(&self, agent_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET error_message = NULL WHERE id = ?1",
                [agent_id],
            )
        })?;
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
    fn ensuring_an_agent_in_a_shared_project_does_not_recreate_its_old_project() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name) VALUES ('shared', 'Shared')",
                [],
            )?;
            conn.execute(
                "INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'shared')",
                [],
            )
        })
        .unwrap();
        let registry = AgentRegistry::new(db.clone());

        let agent = registry.ensure("atlas", "native").unwrap();

        assert_eq!(agent.project_id.as_deref(), Some("shared"));
        let shadow_project: bool = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id = 'atlas')",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert!(!shadow_project);
    }

    #[test]
    fn creating_an_agent_reserves_its_existing_project_and_fails_closed() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name) VALUES ('shared', 'Shared')",
                [],
            )
        })
        .unwrap();
        let registry = AgentRegistry::new(db.clone());

        let agent = registry
            .create_in_project("reviewer", "codex", "shared")
            .unwrap();
        assert_eq!(agent.project_id.as_deref(), Some("shared"));
        let shadow_project = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id = 'reviewer')",
                    [],
                    |row| row.get::<_, bool>(0),
                )
            })
            .unwrap();
        assert!(!shadow_project);

        let missing = registry.create_in_project("orphan", "codex", "deleted");
        assert!(matches!(missing, Err(Error::ProjectNotFound { .. })));
        assert!(matches!(
            registry.get("orphan"),
            Err(Error::AgentNotFound { .. })
        ));
        let orphan_project = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id = 'orphan')",
                    [],
                    |row| row.get::<_, bool>(0),
                )
            })
            .unwrap();
        assert!(!orphan_project);
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
        let registry = AgentRegistry::new(db.clone());

        registry.ensure("atlas", "generic").unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversations (id, title, project_id)
                 VALUES ('shared', 'Shared', 'atlas')",
                [],
            )?;
            conn.execute(
                "INSERT INTO conversation_participants
                 (conversation_id, participant_type, participant_id)
                 VALUES ('shared', 'agent', 'atlas')",
                [],
            )
        })
        .unwrap();
        let schedules = crate::tasks::scheduler::ScheduleManager::new(db.clone());
        let wakeup = schedules
            .create_one_shot(&crate::tasks::scheduler::CreateOneShotSchedule {
                name: "Check the room".into(),
                run_at: None,
                delay_seconds: Some(60),
                agent_id: "atlas".into(),
                title: "Check the room".into(),
                description: Some("Return to the conversation.".into()),
                continuation_task_id: None,
                conversation_id: Some("shared".into()),
            })
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversation_turns
                 (id, conversation_id, agent_id, status, started_at)
                 VALUES ('running-turn', 'shared', 'atlas', 'running', CURRENT_TIMESTAMP)",
                [],
            )
        })
        .unwrap();
        let mut interrupted = Vec::new();
        registry
            .delete_with_running_conversation_turns("atlas", |turn_id| {
                interrupted.push(turn_id.to_string())
            })
            .unwrap();
        assert_eq!(interrupted, ["running-turn"]);
        assert!(registry.get("atlas").is_err());
        assert!(matches!(
            schedules.get(&wakeup.id),
            Err(Error::ScheduleNotFound { .. })
        ));
        let memberships: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_participants
                     WHERE participant_type = 'agent' AND participant_id = 'atlas'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(memberships, 0);
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

        registry.clear_error("atlas").unwrap();
        let recovered = registry.get("atlas").unwrap();
        assert_eq!(recovered.status, "error");
        assert!(recovered.error_message.is_none());
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
