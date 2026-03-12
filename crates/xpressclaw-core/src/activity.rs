use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::db::Database;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub id: i64,
    pub timestamp: String,
    pub event_type: String,
    pub agent_id: Option<String>,
    /// Serialized as `event_data` to match DB column and frontend API contract.
    #[serde(rename = "event_data")]
    pub data: Option<serde_json::Value>,
    pub session_id: Option<String>,
}

/// Activity logger for observability.
pub struct ActivityManager {
    db: Arc<Database>,
}

impl ActivityManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn log(
        &self,
        event_type: &str,
        agent_id: Option<&str>,
        data: Option<&serde_json::Value>,
        session_id: Option<&str>,
    ) -> Result<ActivityEvent> {
        let data_json = data.map(|d| d.to_string());
        let conn = self.db.conn();

        conn.execute(
            "INSERT INTO activity_logs (agent_id, event_type, event_data, session_id) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![agent_id, event_type, data_json, session_id],
        )?;

        let id = conn.last_insert_rowid();
        let mut stmt = conn.prepare("SELECT * FROM activity_logs WHERE id = ?1")?;
        let event = stmt
            .query_row([id], |row| Ok(row_to_event(row)))
            .map_err(|e| Error::Database(e.to_string()))??;

        debug!(event_type, ?agent_id, "logged activity");
        Ok(event)
    }

    pub fn get_recent(&self, limit: i64) -> Result<Vec<ActivityEvent>> {
        let conn = self.db.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM activity_logs ORDER BY timestamp DESC LIMIT ?1")?;

        let events = stmt
            .query_map([limit], |row| Ok(row_to_event(row)))
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }

    pub fn get_by_agent(&self, agent_id: &str, limit: i64) -> Result<Vec<ActivityEvent>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT * FROM activity_logs WHERE agent_id = ?1 ORDER BY timestamp DESC LIMIT ?2",
        )?;

        let events = stmt
            .query_map(rusqlite::params![agent_id, limit], |row| {
                Ok(row_to_event(row))
            })
            .map_err(|e| Error::Database(e.to_string()))?
            .filter_map(|r| r.ok())
            .filter_map(|r| r.ok())
            .collect();

        Ok(events)
    }
}

fn row_to_event(row: &rusqlite::Row) -> Result<ActivityEvent> {
    let data_str: Option<String> = row.get("event_data")?;
    let data = data_str
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    Ok(ActivityEvent {
        id: row.get("id")?,
        timestamp: row.get("timestamp")?,
        event_type: row.get("event_type")?,
        agent_id: row.get("agent_id")?,
        data,
        session_id: row.get("session_id")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_and_retrieve() {
        let db = Arc::new(Database::open_memory().unwrap());
        let mgr = ActivityManager::new(db);

        let data = serde_json::json!({"task_id": "t1", "title": "Test"});
        let event = mgr
            .log("task.created", Some("atlas"), Some(&data), None)
            .unwrap();

        assert_eq!(event.event_type, "task.created");
        assert_eq!(event.agent_id.as_deref(), Some("atlas"));

        let recent = mgr.get_recent(10).unwrap();
        assert_eq!(recent.len(), 1);

        let by_agent = mgr.get_by_agent("atlas", 10).unwrap();
        assert_eq!(by_agent.len(), 1);
    }
}
