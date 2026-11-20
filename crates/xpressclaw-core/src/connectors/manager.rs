use std::sync::Arc;

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::projects::ensure_project_accepts_work;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A connector record as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorRecord {
    pub id: String,
    pub name: String,
    pub connector_type: String,
    pub config: Value,
    pub enabled: bool,
    pub status: String,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A channel record as stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelRecord {
    pub id: String,
    pub connector_id: String,
    pub name: String,
    pub channel_type: String,
    pub config: Value,
    pub agent_id: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

/// Request to create a new connector.
#[derive(Debug, Deserialize)]
pub struct CreateConnector {
    pub name: String,
    pub connector_type: String,
    pub config: Value,
}

/// Request to create a new channel on a connector.
#[derive(Debug, Deserialize)]
pub struct CreateChannel {
    pub name: String,
    pub channel_type: String,
    pub config: Value,
    pub agent_id: Option<String>,
}

// ---------------------------------------------------------------------------
// ConnectorManager
// ---------------------------------------------------------------------------

/// Manages CRUD operations for connectors and their channels in the database.
pub struct ConnectorManager {
    db: Arc<Database>,
}

impl ConnectorManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Create a new connector.
    pub fn create(&self, req: &CreateConnector) -> Result<ConnectorRecord> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let config_json = serde_json::to_string(&req.config)
            .map_err(|e| Error::Connector(format!("failed to serialize config: {e}")))?;

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO connectors (id, name, connector_type, config, enabled, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 1, 'stopped', ?5, ?5)",
                rusqlite::params![id, req.name, req.connector_type, config_json, now],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        self.get(&id)
    }

    /// Get a connector by ID.
    pub fn get(&self, id: &str) -> Result<ConnectorRecord> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM connectors WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_connector(row)))
                .map_err(|_| Error::ConnectorNotFound { id: id.to_string() })
        })
    }

    /// List all connectors.
    pub fn list(&self) -> Result<Vec<ConnectorRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM connectors ORDER BY created_at DESC")
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([], |row| Ok(row_to_connector(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// Update a connector's config and enabled status.
    pub fn update(&self, id: &str, config: Value, enabled: bool) -> Result<ConnectorRecord> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let config_json = serde_json::to_string(&config)
            .map_err(|e| Error::Connector(format!("failed to serialize config: {e}")))?;

        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE connectors SET config = ?1, enabled = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![config_json, enabled as i32, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::ConnectorNotFound { id: id.to_string() });
        }
        self.get(id)
    }

    /// Delete a connector and all its channels.
    pub fn delete(&self, id: &str) -> Result<()> {
        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM connector_channels WHERE connector_id = ?1",
                [id],
            )
            .map_err(|e| Error::Database(e.to_string()))?;

            conn.execute("DELETE FROM connectors WHERE id = ?1", [id])
                .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::ConnectorNotFound { id: id.to_string() });
        }
        Ok(())
    }

    /// Update a connector's runtime status and optional error message.
    pub fn set_status(&self, id: &str, status: &str, error_msg: Option<&str>) -> Result<()> {
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE connectors SET status = ?1, error_message = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![status, error_msg, now, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::ConnectorNotFound { id: id.to_string() });
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Channel operations
    // -----------------------------------------------------------------------

    /// Create a new channel on a connector.
    pub fn create_channel(&self, connector_id: &str, req: &CreateChannel) -> Result<ChannelRecord> {
        // Verify connector exists
        let _ = self.get(connector_id)?;

        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let config_json = serde_json::to_string(&req.config)
            .map_err(|e| Error::Connector(format!("failed to serialize channel config: {e}")))?;

        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_agent_accepts_channel_binding(&transaction, req.agent_id.as_deref())?;
            transaction.execute(
                "INSERT INTO connector_channels (id, connector_id, name, channel_type, config, agent_id, enabled, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)",
                rusqlite::params![
                    id,
                    connector_id,
                    req.name,
                    req.channel_type,
                    config_json,
                    req.agent_id,
                    now,
                ],
            )?;
            transaction.commit()?;
            Ok::<(), Error>(())
        })?;

        self.get_channel(&id)
    }

    /// List all channels for a given connector.
    pub fn list_channels(&self, connector_id: &str) -> Result<Vec<ChannelRecord>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT * FROM connector_channels WHERE connector_id = ?1 ORDER BY created_at DESC",
                )
                .map_err(|e| Error::Database(e.to_string()))?;

            let records = stmt
                .query_map([connector_id], |row| Ok(row_to_channel(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(records)
        })
    }

    /// Get a channel by ID.
    pub fn get_channel(&self, id: &str) -> Result<ChannelRecord> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM connector_channels WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_channel(row)))
                .map_err(|_| Error::ChannelNotFound { id: id.to_string() })
        })
    }

    /// Update a channel's agent binding.
    pub fn update_channel(&self, id: &str, agent_id: Option<&str>) -> Result<ChannelRecord> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_agent_accepts_channel_binding(&transaction, agent_id)?;
            let affected = transaction.execute(
                "UPDATE connector_channels SET agent_id = ?1 WHERE id = ?2",
                rusqlite::params![agent_id, id],
            )?;
            if affected == 0 {
                return Err(Error::ChannelNotFound { id: id.to_string() });
            }
            transaction.commit()?;
            Ok::<(), Error>(())
        })?;
        self.get_channel(id)
    }

    /// Bind a direct connector channel to the Conversation created for it.
    ///
    /// The Conversation is created in its own transaction, so this follow-up
    /// write must revalidate every referenced row while holding a SQLite
    /// writer reservation. If Project deletion wins first, the marker or
    /// missing rows reject the stale binding. If this transaction wins first,
    /// the cascade runs afterward and removes the binding with its Project.
    pub(crate) fn bind_channel_conversation(
        &self,
        conversation_id: &str,
        channel_id: &str,
        agent_id: &str,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let conversation_project = transaction
                .query_row(
                    "SELECT project_id FROM conversations WHERE id = ?1",
                    [conversation_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::ConversationNotFound {
                    id: conversation_id.to_string(),
                })?;
            let agent_project = transaction
                .query_row(
                    "SELECT project_id FROM agents WHERE id = ?1",
                    [agent_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::AgentNotFound {
                    name: agent_id.to_string(),
                })?;
            if conversation_project != agent_project {
                return Err(Error::Conversation(format!(
                    "Conversation '{conversation_id}' and Agent '{agent_id}' belong to different Projects"
                )));
            }
            let is_participant = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM conversation_participants
                    WHERE conversation_id = ?1
                      AND participant_type = 'agent'
                      AND participant_id = ?2
                 )",
                rusqlite::params![conversation_id, agent_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !is_participant {
                return Err(Error::Conversation(format!(
                    "Agent '{agent_id}' is not a participant in Conversation '{conversation_id}'"
                )));
            }
            let channel_agent = transaction
                .query_row(
                    "SELECT agent_id FROM connector_channels WHERE id = ?1",
                    [channel_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::ChannelNotFound {
                    id: channel_id.to_string(),
                })?;
            if channel_agent.as_deref() != Some(agent_id) {
                return Err(Error::Connector(format!(
                    "channel '{channel_id}' is no longer assigned to Agent '{agent_id}'"
                )));
            }
            if let Some(project_id) = agent_project {
                ensure_project_accepts_work(&transaction, &project_id)?;
            }
            transaction.execute(
                "INSERT INTO conversation_channel_bindings
                    (conversation_id, channel_id, agent_id)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(channel_id, agent_id) DO UPDATE SET
                    conversation_id = excluded.conversation_id,
                    created_at = CURRENT_TIMESTAMP",
                rusqlite::params![conversation_id, channel_id, agent_id],
            )?;
            transaction.commit()?;
            Ok::<(), Error>(())
        })
    }

    /// Record an incoming connector event.
    pub fn record_event(
        &self,
        connector_id: &str,
        channel_id: &str,
        event_type: &str,
        payload: &Value,
    ) -> Result<()> {
        let payload_json = serde_json::to_string(payload)
            .map_err(|e| Error::Connector(format!("failed to serialize payload: {e}")))?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO connector_events (connector_id, channel_id, event_type, payload)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![connector_id, channel_id, event_type, payload_json],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        Ok(())
    }

    /// Delete a channel.
    pub fn delete_channel(&self, id: &str) -> Result<()> {
        let affected = self.db.with_conn(|conn| {
            conn.execute("DELETE FROM connector_channels WHERE id = ?1", [id])
                .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::ChannelNotFound { id: id.to_string() });
        }
        Ok(())
    }
}

/// Validate an optional direct Agent binding while holding the same SQLite
/// writer reservation used to persist it. This serializes channel assignment
/// with Project deletion: an assignment that commits first is cleared by the
/// cascade, while one that starts afterward observes the deletion marker or
/// missing Agent and fails closed.
fn ensure_agent_accepts_channel_binding(
    conn: &rusqlite::Connection,
    agent_id: Option<&str>,
) -> Result<()> {
    let Some(agent_id) = agent_id else {
        return Ok(());
    };
    let project_id = conn
        .query_row(
            "SELECT project_id FROM agents WHERE id = ?1",
            [agent_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .ok_or_else(|| Error::AgentNotFound {
            name: agent_id.to_string(),
        })?;
    if let Some(project_id) = project_id {
        ensure_project_accepts_work(conn, &project_id)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Row mapping helpers
// ---------------------------------------------------------------------------

fn row_to_connector(row: &rusqlite::Row) -> ConnectorRecord {
    let config_str: String = row.get("config").unwrap_or_default();
    let config: Value =
        serde_json::from_str(&config_str).unwrap_or(Value::Object(Default::default()));

    ConnectorRecord {
        id: row.get("id").unwrap_or_default(),
        name: row.get("name").unwrap_or_default(),
        connector_type: row.get("connector_type").unwrap_or_default(),
        config,
        enabled: row.get::<_, i32>("enabled").unwrap_or(1) != 0,
        status: row.get("status").unwrap_or_default(),
        error_message: row.get("error_message").unwrap_or_default(),
        created_at: row.get("created_at").unwrap_or_default(),
        updated_at: row.get("updated_at").unwrap_or_default(),
    }
}

fn row_to_channel(row: &rusqlite::Row) -> ChannelRecord {
    let config_str: String = row.get("config").unwrap_or_default();
    let config: Value =
        serde_json::from_str(&config_str).unwrap_or(Value::Object(Default::default()));

    ChannelRecord {
        id: row.get("id").unwrap_or_default(),
        connector_id: row.get("connector_id").unwrap_or_default(),
        name: row.get("name").unwrap_or_default(),
        channel_type: row.get("channel_type").unwrap_or_default(),
        config,
        agent_id: row.get("agent_id").unwrap_or_default(),
        enabled: row.get::<_, i32>("enabled").unwrap_or(1) != 0,
        created_at: row.get("created_at").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::AgentRegistry;
    use crate::conversations::{ConversationManager, CreateConversation};
    use crate::projects::{CreateProject, ProjectManager};

    #[test]
    fn channel_bindings_reject_deleting_and_deleted_agents() {
        let db = Arc::new(Database::open_memory().unwrap());
        let projects = ProjectManager::new(db.clone());
        let project = projects
            .create(&CreateProject {
                name: "Project One".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        AgentRegistry::new(db.clone())
            .create_in_project("project-agent", "codex", &project.id)
            .unwrap();
        let manager = ConnectorManager::new(db);
        let connector = manager
            .create(&CreateConnector {
                name: "Local webhook".into(),
                connector_type: "webhook".into(),
                config: Value::Object(Default::default()),
            })
            .unwrap();
        let channel = manager
            .create_channel(
                &connector.id,
                &CreateChannel {
                    name: "Inbox".into(),
                    channel_type: "both".into(),
                    config: Value::Object(Default::default()),
                    agent_id: None,
                },
            )
            .unwrap();

        projects.begin_cascade(&project.id).unwrap();

        let update_error = manager
            .update_channel(&channel.id, Some("project-agent"))
            .unwrap_err();
        assert!(update_error.to_string().contains("being deleted"));
        let create_error = manager
            .create_channel(
                &connector.id,
                &CreateChannel {
                    name: "Late inbox".into(),
                    channel_type: "both".into(),
                    config: Value::Object(Default::default()),
                    agent_id: Some("project-agent".into()),
                },
            )
            .unwrap_err();
        assert!(create_error.to_string().contains("being deleted"));
        assert_eq!(manager.get_channel(&channel.id).unwrap().agent_id, None);
        assert_eq!(manager.list_channels(&connector.id).unwrap().len(), 1);

        projects.finish_cascade(&project.id).unwrap();
        let deleted_error = manager
            .update_channel(&channel.id, Some("project-agent"))
            .unwrap_err();
        assert!(matches!(deleted_error, Error::AgentNotFound { .. }));
        assert_eq!(manager.get_channel(&channel.id).unwrap().agent_id, None);
    }

    #[test]
    fn channel_conversation_bindings_serialize_with_project_cascade() {
        let db = Arc::new(Database::open_memory().unwrap());
        let projects = ProjectManager::new(db.clone());
        let project = projects
            .create(&CreateProject {
                name: "Project One".into(),
                description: None,
                icon: None,
            })
            .unwrap();
        AgentRegistry::new(db.clone())
            .create_in_project("project-agent", "codex", &project.id)
            .unwrap();
        let manager = ConnectorManager::new(db.clone());
        let connector = manager
            .create(&CreateConnector {
                name: "Local webhook".into(),
                connector_type: "webhook".into(),
                config: Value::Object(Default::default()),
            })
            .unwrap();
        let channel = manager
            .create_channel(
                &connector.id,
                &CreateChannel {
                    name: "Inbox".into(),
                    channel_type: "both".into(),
                    config: Value::Object(Default::default()),
                    agent_id: Some("project-agent".into()),
                },
            )
            .unwrap();
        let conversations = ConversationManager::new(db.clone());
        let create_conversation = || {
            conversations
                .create(&CreateConversation {
                    title: Some("#inbox".into()),
                    icon: None,
                    participant_ids: vec!["project-agent".into()],
                })
                .unwrap()
        };
        let bound_before_cascade = create_conversation();
        let stale_unbound = create_conversation();

        // If binding wins the writer first, the later cascade owns and
        // removes it.
        manager
            .bind_channel_conversation(&bound_before_cascade.id, &channel.id, "project-agent")
            .unwrap();
        let binding_count = || {
            db.with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_channel_bindings",
                    [],
                    |row| row.get::<_, i64>(0),
                )
            })
            .unwrap()
        };
        assert_eq!(binding_count(), 1);

        projects.begin_cascade(&project.id).unwrap();

        // If deletion wins the writer first, the stale follow-up sees the
        // marker and cannot insert a second binding.
        let deleting_error = manager
            .bind_channel_conversation(&stale_unbound.id, &channel.id, "project-agent")
            .unwrap_err();
        assert!(deleting_error.to_string().contains("being deleted"));
        assert_eq!(binding_count(), 1);

        projects.finish_cascade(&project.id).unwrap();
        assert_eq!(binding_count(), 0);

        // A descheduled router that resumes after finalization cannot recreate
        // a binding to the deleted Conversation or Agent.
        let deleted_error = manager
            .bind_channel_conversation(&stale_unbound.id, &channel.id, "project-agent")
            .unwrap_err();
        assert!(matches!(deleted_error, Error::ConversationNotFound { .. }));
        assert_eq!(binding_count(), 0);
    }
}
