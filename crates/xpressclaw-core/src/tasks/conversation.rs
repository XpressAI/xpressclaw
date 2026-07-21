use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::Result;
use crate::tasks::attachments::DecodedImageAttachment;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskMessageAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMessage {
    pub id: i64,
    pub task_id: String,
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub attachments: Vec<TaskMessageAttachment>,
}

#[derive(Debug, Clone)]
pub struct PromptImageAttachment {
    pub name: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PromptTaskMessage {
    pub content: String,
    pub attachments: Vec<PromptImageAttachment>,
}

#[derive(Debug, Clone)]
pub struct TaskMessageAttachmentData {
    pub name: String,
    pub mime_type: String,
    pub data: Vec<u8>,
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
        self.add_message_with_attachments(task_id, role, content, &[])
    }

    pub fn add_message_with_attachments(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        attachments: &[DecodedImageAttachment],
    ) -> Result<TaskMessage> {
        let mut conn = self.db.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO task_messages (task_id, role, content) VALUES (?1, ?2, ?3)",
            rusqlite::params![task_id, role, content],
        )?;

        let id = tx.last_insert_rowid();
        let mut stored_attachments = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let attachment_id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO task_message_attachments
                    (id, message_id, name, mime_type, data, size)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    attachment_id,
                    id,
                    attachment.name,
                    attachment.mime_type,
                    attachment.data,
                    attachment.data.len() as i64,
                ],
            )?;
            stored_attachments.push(TaskMessageAttachment {
                id: attachment_id,
                name: attachment.name.clone(),
                mime_type: attachment.mime_type.clone(),
                size: attachment.data.len(),
            });
        }
        let (task_id, role, content, timestamp) = tx.query_row(
            "SELECT task_id, role, content, timestamp FROM task_messages WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        tx.commit()?;

        Ok(TaskMessage {
            id,
            task_id,
            role,
            content,
            timestamp,
            attachments: stored_attachments,
        })
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
            conn.prepare("SELECT * FROM task_messages WHERE task_id = ?1 ORDER BY id ASC")?;

        let mut messages: Vec<TaskMessage> = stmt
            .query_map([task_id], |row| {
                Ok(TaskMessage {
                    id: row.get("id")?,
                    task_id: row.get("task_id")?,
                    role: row.get("role")?,
                    content: row.get("content")?,
                    timestamp: row.get("timestamp")?,
                    attachments: Vec::new(),
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);

        let mut attachment_stmt = conn.prepare(
            "SELECT id, name, mime_type, size
             FROM task_message_attachments WHERE message_id = ?1 ORDER BY created_at, id",
        )?;
        for message in &mut messages {
            message.attachments = attachment_stmt
                .query_map([message.id], |row| {
                    Ok(TaskMessageAttachment {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        mime_type: row.get(2)?,
                        size: row.get::<_, i64>(3)? as usize,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
        }

        Ok(messages)
    }

    /// Load user text and binary images that arrived after the previous turn began.
    pub fn get_user_messages_since(
        &self,
        task_id: &str,
        since: Option<&str>,
    ) -> Result<Vec<PromptTaskMessage>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, content FROM task_messages
             WHERE task_id = ?1 AND role = 'user'
               AND (?2 IS NULL OR timestamp >= ?2)
             ORDER BY id ASC",
        )?;
        let rows: Vec<(i64, String)> = stmt
            .query_map(rusqlite::params![task_id, since], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        drop(stmt);

        let mut attachment_stmt = conn.prepare(
            "SELECT name, mime_type, data
             FROM task_message_attachments WHERE message_id = ?1 ORDER BY created_at, id",
        )?;
        rows.into_iter()
            .map(|(message_id, content)| {
                let attachments = attachment_stmt
                    .query_map([message_id], |row| {
                        Ok(PromptImageAttachment {
                            name: row.get(0)?,
                            mime_type: row.get(1)?,
                            data: row.get(2)?,
                        })
                    })?
                    .collect::<std::result::Result<_, _>>()?;
                Ok(PromptTaskMessage {
                    content,
                    attachments,
                })
            })
            .collect()
    }

    pub fn get_attachment(
        &self,
        task_id: &str,
        message_id: i64,
        attachment_id: &str,
    ) -> Result<Option<TaskMessageAttachmentData>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT a.name, a.mime_type, a.data
             FROM task_message_attachments a
             JOIN task_messages m ON m.id = a.message_id
             WHERE m.task_id = ?1 AND m.id = ?2 AND a.id = ?3",
        )?;
        let mut rows = stmt.query(rusqlite::params![task_id, message_id, attachment_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(TaskMessageAttachmentData {
            name: row.get(0)?,
            mime_type: row.get(1)?,
            data: row.get(2)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::board::{CreateTask, TaskBoard};

    #[test]
    fn stores_attachment_metadata_and_loads_prompt_bytes() {
        let db = Arc::new(Database::open_memory().unwrap());
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Inspect screenshot".to_string(),
                ..Default::default()
            })
            .unwrap();
        let conversation = TaskConversation::new(db);
        let image = DecodedImageAttachment {
            name: "screen.png".to_string(),
            mime_type: "image/png".to_string(),
            data: b"\x89PNG\r\n\x1a\nbytes".to_vec(),
        };

        let created = conversation
            .add_message_with_attachments(&task.id, "user", "What is wrong?", &[image])
            .unwrap();
        assert_eq!(created.attachments.len(), 1);
        let messages = conversation.get_messages(&task.id).unwrap();
        assert_eq!(messages[0].attachments[0].name, "screen.png");
        assert_eq!(messages[0].attachments[0].size, 13);

        let prompt = conversation
            .get_user_messages_since(&task.id, None)
            .unwrap();
        assert_eq!(prompt[0].content, "What is wrong?");
        assert_eq!(prompt[0].attachments[0].data, b"\x89PNG\r\n\x1a\nbytes");

        let downloaded = conversation
            .get_attachment(&task.id, created.id, &created.attachments[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(downloaded.mime_type, "image/png");
        assert_eq!(downloaded.data, b"\x89PNG\r\n\x1a\nbytes");
    }
}
