use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};

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
            conn.execute(
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
            )
            .map_err(|e| Error::Database(e.to_string()))
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
        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE connector_channels SET agent_id = ?1 WHERE id = ?2",
                rusqlite::params![agent_id, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::ChannelNotFound { id: id.to_string() });
        }
        self.get_channel(id)
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
