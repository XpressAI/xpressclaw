use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMessage {
    pub id: i64,
    pub task_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
}

/// Manages conversation threads for tasks.
pub struct TaskConversation {
    db: Arc<Database>,
}

impl TaskConversation {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn add_message(&self, task_id: &str, role: &str, content: &str) -> Result<TaskMessage> {
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO task_messages (task_id, role, content) VALUES (?1, ?2, ?3)",
            rusqlite::params![task_id, role, content],
        )?;

        let id = conn.last_insert_rowid();
        let mut stmt = conn.prepare("SELECT * FROM task_messages WHERE id = ?1")?;
        let msg = stmt
            .query_row([id], |row| {
                Ok(TaskMessage {
                    id: row.get("id")?,
                    task_id: row.get("task_id")?,
                    role: row.get("role")?,
                    content: row.get("content")?,
                    timestamp: row.get("timestamp")?,
                })
            })
            .map_err(|e| Error::Database(e.to_string()))?;

        Ok(msg)
    }

    pub fn update_message_content(&self, message_id: i64, content: &str) -> Result<()> {
        let conn = self.db.conn();
        conn.execute(
            "UPDATE task_messages SET content = ?1 WHERE id = ?2",
            rusqlite::params![content, message_id],
        )?;
        Ok(())
    }

    pub fn get_messages(&self, task_id: &str) -> Result<Vec<TaskMessage>> {
        let conn = self.db.conn();
        let mut stmt =
            conn.prepare("SELECT * FROM task_messages WHERE task_id = ?1 ORDER BY timestamp ASC")?;

        let messages = stmt
            .query_map([task_id], |row| {
                Ok(TaskMessage {
                    id: row.get("id")?,
                    task_id: row.get("task_id")?,
                    role: row.get("role")?,
                    content: row.get("content")?,
                    timestamp: row.get("timestamp")?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(messages)
    }
}
