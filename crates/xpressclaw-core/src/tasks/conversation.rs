use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::message_artifacts::PublishedFileAttachment;
use crate::projects::ensure_project_accepts_work;
use crate::tasks::attachments::DecodedImageAttachment;
use crate::tasks::queue::{QueueItem, TaskQueue};
use crate::visualizations::{
    store_task_message_visualizations, MessageVisualization, PreparedVisualization,
    VisualizationManager,
};

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
    #[serde(default)]
    pub visualizations: Vec<MessageVisualization>,
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

struct MessageExtras<'a> {
    image_attachments: &'a [DecodedImageAttachment],
    published_files: &'a [PublishedFileAttachment],
    attempt_id: Option<&'a str>,
    visualizations: &'a [PreparedVisualization],
}

pub(crate) struct FinalAssistantAttempt<'a> {
    pub(crate) task_id: &'a str,
    pub(crate) queue_id: i64,
    pub(crate) attempt_id: &'a str,
    pub(crate) completion_summary: &'a str,
    pub(crate) content: &'a str,
    pub(crate) visualizations: &'a [PreparedVisualization],
    pub(crate) published_files: &'a [PublishedFileAttachment],
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
        self.add_message_with_attachments_and_visualizations(
            task_id,
            role,
            content,
            MessageExtras {
                image_attachments: attachments,
                published_files: &[],
                attempt_id: None,
                visualizations: &[],
            },
        )
    }

    /// Commit a user-authored task message and its response attempt in one
    /// immediate transaction. A workflow continuation can therefore observe
    /// either both records or neither; it cannot claim an unqueued user
    /// message as part of its fixed-prompt response.
    pub fn add_user_message_with_attachments_and_enqueue(
        &self,
        task_id: &str,
        agent_id: Option<&str>,
        content: &str,
        attachments: &[DecodedImageAttachment],
    ) -> Result<(TaskMessage, Option<QueueItem>)> {
        let conn = self.db.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let message = Self::insert_message_in_transaction(
            &tx,
            task_id,
            "user",
            content,
            MessageExtras {
                image_attachments: attachments,
                published_files: &[],
                attempt_id: None,
                visualizations: &[],
            },
        )?;
        let continuation = agent_id
            .map(|agent_id| {
                TaskQueue::enqueue_continuation_for_message_in_transaction(
                    &tx,
                    task_id,
                    agent_id,
                    message.id,
                    &message.timestamp,
                )
            })
            .transpose()?
            .flatten();
        tx.commit()?;
        Ok((message, continuation))
    }

    /// Persist a final assistant response and its copied visualizations/files
    /// in one transaction. A process crash can therefore expose neither a
    /// dangling content reference nor an ownerless artifact.
    pub fn add_final_assistant_message(
        &self,
        task_id: &str,
        content: &str,
        attempt_id: &str,
        visualizations: &[PreparedVisualization],
        published_files: &[PublishedFileAttachment],
    ) -> Result<TaskMessage> {
        self.add_message_with_attachments_and_visualizations(
            task_id,
            "assistant",
            content,
            MessageExtras {
                image_attachments: &[],
                published_files,
                attempt_id: Some(attempt_id),
                visualizations,
            },
        )
    }

    fn add_message_with_attachments_and_visualizations(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        extras: MessageExtras<'_>,
    ) -> Result<TaskMessage> {
        let conn = self.db.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let message = Self::insert_message_in_transaction(&tx, task_id, role, content, extras)?;
        tx.commit()?;
        Ok(message)
    }

    fn insert_message_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        task_id: &str,
        role: &str,
        content: &str,
        extras: MessageExtras<'_>,
    ) -> Result<TaskMessage> {
        let project_id = tx
            .query_row(
                "SELECT project_id FROM tasks WHERE id = ?1",
                [task_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or_else(|| Error::TaskNotFound {
                id: task_id.to_string(),
            })?;
        if let Some(project_id) = project_id.as_deref() {
            ensure_project_accepts_work(tx, project_id)?;
        }
        tx.execute(
            "INSERT INTO task_messages (task_id, role, content) VALUES (?1, ?2, ?3)",
            rusqlite::params![task_id, role, content],
        )?;

        let id = tx.last_insert_rowid();
        let mut stored_attachments =
            Vec::with_capacity(extras.image_attachments.len() + extras.published_files.len());
        for attachment in extras.image_attachments {
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
        for attachment in extras.published_files {
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
        let stored_visualizations =
            store_task_message_visualizations(tx, id, extras.attempt_id, extras.visualizations)?;

        Ok(TaskMessage {
            id,
            task_id,
            role,
            content,
            timestamp,
            attachments: stored_attachments,
            visualizations: stored_visualizations,
        })
    }

    /// Insert a plain task-chat message inside a caller-owned transaction.
    /// Workflow continuation steps use this to commit the fixed prompt, its
    /// step execution, and the queued response cycle atomically.
    pub(crate) fn insert_text_message_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        task_id: &str,
        role: &str,
        content: &str,
    ) -> Result<TaskMessage> {
        Self::insert_message_in_transaction(
            transaction,
            task_id,
            role,
            content,
            MessageExtras {
                image_attachments: &[],
                published_files: &[],
                attempt_id: None,
                visualizations: &[],
            },
        )
    }

    /// Commit the final Task reply and terminal attempt/queue state together.
    /// Cancellation and completion therefore have one SQLite serialization
    /// point, and a failed attachment/message write leaves the running work
    /// retryable instead of exposing a completed attempt without its reply.
    pub(crate) fn complete_final_assistant_attempt(
        &self,
        completion: FinalAssistantAttempt<'_>,
    ) -> Result<Option<TaskMessage>> {
        let conn = self.db.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let attempt = tx
            .query_row(
                "SELECT attempt.session_id, attempt.runner
                 FROM work_attempts attempt
                 JOIN task_queue queue
                   ON queue.id = ?1 AND queue.attempt_id = attempt.id
                 WHERE attempt.id = ?2 AND attempt.task_id = ?3
                   AND attempt.queue_id = ?1
                   AND attempt.status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')
                   AND queue.status = 'running'",
                rusqlite::params![
                    completion.queue_id,
                    completion.attempt_id,
                    completion.task_id
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((session_id, runner)) = attempt else {
            tx.commit()?;
            return Ok(None);
        };

        let message = Self::insert_message_in_transaction(
            &tx,
            completion.task_id,
            "assistant",
            completion.content,
            MessageExtras {
                image_attachments: &[],
                published_files: completion.published_files,
                attempt_id: Some(completion.attempt_id),
                visualizations: completion.visualizations,
            },
        )?;
        let attempt_updated = tx.execute(
            "UPDATE work_attempts
             SET status = 'completed', completed_at = CURRENT_TIMESTAMP,
                 result = ?1, error_message = NULL
             WHERE id = ?2
               AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')",
            rusqlite::params![completion.content, completion.attempt_id],
        )?;
        let queue_updated = tx.execute(
            "UPDATE task_queue
             SET status = 'completed', completed_at = CURRENT_TIMESTAMP,
                 harness_response = ?1
             WHERE id = ?2 AND attempt_id = ?3 AND status = 'running'",
            rusqlite::params![
                completion.content,
                completion.queue_id,
                completion.attempt_id
            ],
        )?;
        if attempt_updated != 1 || queue_updated != 1 {
            return Err(Error::Task(
                "native completion lost its running attempt or queue lease".into(),
            ));
        }
        tx.execute(
            "UPDATE tasks SET active_attempt_id = NULL, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1 AND active_attempt_id = ?2",
            rusqlite::params![completion.task_id, completion.attempt_id],
        )?;
        tx.execute(
            "UPDATE logical_sessions
             SET status = CASE
                    WHEN EXISTS(
                        SELECT 1 FROM work_attempts
                        WHERE session_id = ?1
                          AND status IN ('preparing', 'running', 'review')
                    ) THEN 'running'
                    WHEN EXISTS(
                        SELECT 1 FROM work_attempts
                        WHERE session_id = ?1 AND status = 'queued'
                    ) THEN 'queued'
                    WHEN EXISTS(
                        SELECT 1 FROM tasks
                        WHERE agent_id = ?1 AND status = 'waiting_for_input'
                    ) THEN 'waiting_for_input'
                    WHEN EXISTS(
                        SELECT 1 FROM tasks
                        WHERE agent_id = ?1 AND status = 'blocked'
                    ) THEN 'blocked'
                    ELSE 'idle'
                 END,
                 latest_summary = ?2, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            rusqlite::params![session_id, completion.completion_summary],
        )?;
        tx.execute(
            "INSERT INTO session_events
             (session_id, attempt_id, task_id, source_type, source_id,
              event_type, summary, payload)
             VALUES (?1, ?2, ?3, 'runner', ?4, 'attempt_completed', ?5, ?6)",
            rusqlite::params![
                session_id,
                completion.attempt_id,
                completion.task_id,
                runner,
                completion.completion_summary,
                json!({ "status": "completed", "error": null }).to_string(),
            ],
        )?;
        tx.commit()?;
        Ok(Some(message))
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
                    visualizations: Vec::new(),
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
        drop(attachment_stmt);
        drop(conn);
        let visualization_manager = VisualizationManager::new(self.db.clone());
        for message in &mut messages {
            message.visualizations = visualization_manager.list_for_task_message(message.id)?;
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
    use crate::agents::registry::AgentRegistry;
    use crate::sessions::SessionManager;
    use crate::tasks::board::{CreateTask, TaskBoard};
    use crate::tasks::queue::TaskQueue;

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

    #[test]
    fn stores_final_presentation_with_its_assistant_message() {
        let db = Arc::new(Database::open_memory().unwrap());
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Build a deck".to_string(),
                ..Default::default()
            })
            .unwrap();
        let conversation = TaskConversation::new(db);
        let file = PublishedFileAttachment {
            name: "Review.pptx".into(),
            mime_type: "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                .into(),
            data: b"pptx bytes".to_vec(),
        };

        let message = conversation
            .add_final_assistant_message(
                &task.id,
                "The checked deck is attached.",
                "attempt-1",
                &[],
                &[file],
            )
            .unwrap();
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].name, "Review.pptx");
        let downloaded = conversation
            .get_attachment(&task.id, message.id, &message.attachments[0].id)
            .unwrap()
            .unwrap();
        assert_eq!(downloaded.data, b"pptx bytes");
    }

    fn running_attempt(db: &Arc<Database>) -> (String, i64, String) {
        AgentRegistry::new(db.clone())
            .ensure("atlas", "generic")
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Build a deck".to_string(),
                agent_id: Some("atlas".to_string()),
                ..Default::default()
            })
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        queue.enqueue(&task.id, "atlas").unwrap();
        let item = queue.claim("atlas").unwrap().unwrap();
        let attempt_id = item.attempt_id.clone().unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(&attempt_id, "running", "Working", None, None)
            .unwrap();
        (task.id, item.id, attempt_id)
    }

    #[test]
    fn final_reply_and_terminal_attempt_state_commit_atomically() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (task_id, queue_id, attempt_id) = running_attempt(&db);
        let file = PublishedFileAttachment {
            name: "Review.pptx".into(),
            mime_type: "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                .into(),
            data: b"pptx bytes".to_vec(),
        };

        let message = TaskConversation::new(db.clone())
            .complete_final_assistant_attempt(FinalAssistantAttempt {
                task_id: &task_id,
                queue_id,
                attempt_id: &attempt_id,
                completion_summary: "Deck complete",
                content: "The deck is attached.",
                visualizations: &[],
                published_files: &[file],
            })
            .unwrap()
            .unwrap();

        assert_eq!(message.attachments.len(), 1);
        assert_eq!(
            SessionManager::new(db.clone())
                .get_attempt(&attempt_id)
                .unwrap()
                .status,
            "completed"
        );
        assert_eq!(
            TaskQueue::new(db.clone()).get(queue_id).unwrap().status,
            "completed"
        );
        let active_attempt = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT active_attempt_id FROM tasks WHERE id = ?1",
                    [&task_id],
                    |row| row.get::<_, Option<String>>(0),
                )
            })
            .unwrap();
        assert!(active_attempt.is_none());
        let completed_events = SessionManager::new(db)
            .list_events("atlas", None, 100)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == "attempt_completed")
            .count();
        assert_eq!(completed_events, 1);
    }

    #[test]
    fn failed_final_reply_write_leaves_attempt_and_queue_running() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (task_id, queue_id, attempt_id) = running_attempt(&db);
        let invalid_visualization = PreparedVisualization {
            reference_index: 0,
            title: "Invalid".into(),
            mode: "normal".into(),
            status: "ready".into(),
            error_code: None,
            content: None,
            content_sha256: None,
            size: None,
        };

        let result = TaskConversation::new(db.clone()).complete_final_assistant_attempt(
            FinalAssistantAttempt {
                task_id: &task_id,
                queue_id,
                attempt_id: &attempt_id,
                completion_summary: "Deck complete",
                content: "The deck is attached.",
                visualizations: &[invalid_visualization],
                published_files: &[],
            },
        );

        assert!(result.is_err());
        assert!(TaskConversation::new(db.clone())
            .get_messages(&task_id)
            .unwrap()
            .is_empty());
        assert_eq!(
            SessionManager::new(db.clone())
                .get_attempt(&attempt_id)
                .unwrap()
                .status,
            "running"
        );
        assert_eq!(TaskQueue::new(db).get(queue_id).unwrap().status, "running");
    }

    #[test]
    fn cancelled_attempt_cannot_publish_a_late_final_reply() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (task_id, queue_id, attempt_id) = running_attempt(&db);
        SessionManager::new(db.clone())
            .transition_attempt(&attempt_id, "cancelled", "Cancelled", None, None)
            .unwrap();

        let message = TaskConversation::new(db.clone())
            .complete_final_assistant_attempt(FinalAssistantAttempt {
                task_id: &task_id,
                queue_id,
                attempt_id: &attempt_id,
                completion_summary: "Too late",
                content: "This must not appear.",
                visualizations: &[],
                published_files: &[],
            })
            .unwrap();

        assert!(message.is_none());
        assert!(TaskConversation::new(db)
            .get_messages(&task_id)
            .unwrap()
            .is_empty());
    }
}
