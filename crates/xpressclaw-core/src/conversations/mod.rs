pub mod event_bus;
pub mod processor;
pub mod runtime;

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::projects::ensure_project_accepts_work;
use crate::tasks::board::{CreateTask, Task, TaskBoard};
use crate::tasks::queue::TaskQueue;
use crate::visualizations::{
    store_conversation_message_visualizations, MessageVisualization, PreparedVisualization,
    VisualizationManager,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub project_id: Option<String>,
    pub title: Option<String>,
    pub icon: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_message_at: Option<String>,
    #[serde(default)]
    pub participants: Vec<Participant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub participant_type: String,
    pub participant_id: String,
    pub joined_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: i64,
    pub conversation_id: String,
    pub sender_type: String,
    pub sender_id: String,
    pub sender_name: Option<String>,
    pub content: String,
    pub message_type: String,
    pub linked_task_id: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationAttachment {
    pub id: String,
    pub message_id: i64,
    pub name: String,
    pub mime_type: String,
    pub size: i64,
    pub source_task_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct NewConversationAttachment {
    pub name: String,
    pub mime_type: String,
    pub data: Vec<u8>,
    pub source_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateConversation {
    pub title: Option<String>,
    pub icon: Option<String>,
    #[serde(default)]
    pub participant_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessage {
    pub sender_type: String,
    pub sender_id: String,
    pub sender_name: Option<String>,
    pub content: String,
    pub message_type: Option<String>,
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationMessage> {
    Ok(ConversationMessage {
        id: row.get("id")?,
        conversation_id: row.get("conversation_id")?,
        sender_type: row.get("sender_type")?,
        sender_id: row.get("sender_id")?,
        sender_name: row.get("sender_name")?,
        content: row.get("content")?,
        message_type: row.get("message_type")?,
        linked_task_id: row.get("linked_task_id").unwrap_or(None),
        metadata: row
            .get::<_, String>("metadata")
            .ok()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_else(|| serde_json::json!({})),
        created_at: row.get("created_at")?,
    })
}

fn row_to_attachment(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationAttachment> {
    Ok(ConversationAttachment {
        id: row.get("id")?,
        message_id: row.get("message_id")?,
        name: row.get("name")?,
        mime_type: row.get("mime_type")?,
        size: row.get("size")?,
        source_task_id: row.get("source_task_id")?,
        created_at: row.get("created_at")?,
    })
}

fn read_attachment(conn: &rusqlite::Connection, id: &str) -> Result<ConversationAttachment> {
    conn.query_row(
        "SELECT id, message_id, name, mime_type, size, source_task_id, created_at
         FROM conversation_message_attachments WHERE id = ?1",
        [id],
        row_to_attachment,
    )
    .map_err(Error::from)
}

fn validate_new_attachments(attachments: &[NewConversationAttachment]) -> Result<()> {
    const MAX_ATTACHMENT_COUNT: usize = 10;
    const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
    if attachments.len() > MAX_ATTACHMENT_COUNT {
        return Err(Error::Conversation(
            "a conversation message may contain at most 10 attachments".into(),
        ));
    }
    let total_size = attachments.iter().try_fold(0usize, |total, attachment| {
        if attachment.name.trim().is_empty() || attachment.name.len() > 255 {
            return Err(Error::Conversation(
                "conversation attachment names must be between 1 and 255 bytes".into(),
            ));
        }
        if attachment.mime_type.trim().is_empty() || attachment.mime_type.len() > 255 {
            return Err(Error::Conversation(
                "conversation attachment MIME types must be between 1 and 255 bytes".into(),
            ));
        }
        total
            .checked_add(attachment.data.len())
            .ok_or_else(|| Error::Conversation("conversation attachments are too large".into()))
    })?;
    if total_size > MAX_ATTACHMENT_BYTES {
        return Err(Error::Conversation(
            "conversation attachments in one message must total 20 MiB or less".into(),
        ));
    }
    Ok(())
}

/// Manages conversations and their messages.
pub struct ConversationManager {
    db: Arc<Database>,
}

impl ConversationManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn create(&self, req: &CreateConversation) -> Result<Conversation> {
        let project_id = req.participant_ids.iter().find_map(|agent_id| {
            self.db.with_conn(|conn| {
                conn.query_row(
                    "SELECT project_id FROM agents WHERE id = ?1",
                    [agent_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
            })
        });
        self.create_in_project(project_id.as_deref(), req)
    }

    pub fn create_in_project(
        &self,
        project_id: Option<&str>,
        req: &CreateConversation,
    ) -> Result<Conversation> {
        let id = Uuid::new_v4().to_string();

        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            if let Some(project_id) = project_id {
                ensure_project_accepts_work(&transaction, project_id)?;
            }
            for agent_id in &req.participant_ids {
                let agent_project = transaction
                    .query_row(
                        "SELECT project_id FROM agents WHERE id = ?1",
                        [agent_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .map_err(|_| Error::AgentNotFound {
                        name: agent_id.clone(),
                    })?;
                if project_id.is_some() && agent_project.as_deref() != project_id {
                    return Err(Error::Conversation(format!(
                        "Agent '{agent_id}' does not belong to this project"
                    )));
                }
            }
            transaction.execute(
                "INSERT INTO conversations (id, title, icon, project_id) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, req.title, req.icon, project_id],
            )?;

            // Add the local user as participant
            transaction.execute(
                "INSERT INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, 'user', 'local')",
                rusqlite::params![id],
            )?;

            // Add requested agent participants
            for agent_id in &req.participant_ids {
                transaction.execute(
                    "INSERT OR IGNORE INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, 'agent', ?2)",
                    rusqlite::params![id, agent_id],
                )?;
            }

            transaction.commit()?;
            Ok::<(), Error>(())
        })?;

        self.get(&id)
    }

    pub fn list(&self, limit: i64) -> Result<Vec<Conversation>> {
        self.list_in_project(None, limit)
    }

    pub fn list_in_project(
        &self,
        project_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Conversation>> {
        self.db.with_conn(|conn| {
            let sql = if project_id.is_some() {
                "SELECT id, project_id, title, icon, created_at, updated_at, last_message_at
                 FROM conversations WHERE project_id = ?1
                 ORDER BY COALESCE(last_message_at, created_at) DESC LIMIT ?2"
            } else {
                "SELECT id, project_id, title, icon, created_at, updated_at, last_message_at
                 FROM conversations
                 ORDER BY COALESCE(last_message_at, created_at) DESC LIMIT ?2"
            };
            let mut stmt = conn.prepare(sql)?;

            let convs: Vec<Conversation> = stmt
                .query_map(rusqlite::params![project_id, limit], |row| {
                    Ok(Conversation {
                        id: row.get("id")?,
                        project_id: row.get("project_id")?,
                        title: row.get("title")?,
                        icon: row.get("icon")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                        last_message_at: row.get("last_message_at")?,
                        participants: vec![],
                    })
                })?
                .filter_map(|r| r.ok())
                .collect();

            // Load participants for each conversation
            let mut result = Vec::with_capacity(convs.len());
            for mut conv in convs {
                conv.participants = self.load_participants(conn, &conv.id)?;
                result.push(conv);
            }

            Ok(result)
        })
    }

    pub fn get(&self, id: &str) -> Result<Conversation> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, project_id, title, icon, created_at, updated_at, last_message_at
                 FROM conversations WHERE id = ?1",
            )?;

            let mut conv = stmt
                .query_row([id], |row| {
                    Ok(Conversation {
                        id: row.get("id")?,
                        project_id: row.get("project_id")?,
                        title: row.get("title")?,
                        icon: row.get("icon")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                        last_message_at: row.get("last_message_at")?,
                        participants: vec![],
                    })
                })
                .map_err(|_| Error::ConversationNotFound { id: id.to_string() })?;

            conv.participants = self.load_participants(conn, id)?;
            Ok(conv)
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.delete_with_running_turns(id, |_| {})
    }

    /// Cancel every live turn and notify the caller before the conversation
    /// row is removed. The callback runs while the write transaction is held,
    /// after turns have been made unpublishable but before cascading deletes,
    /// so the server can signal ACP processes without allowing a late reply or
    /// a newly queued turn to race deletion.
    pub fn delete_with_running_turns<F>(&self, id: &str, mut before_delete: F) -> Result<()>
    where
        F: FnMut(&str),
    {
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let exists = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM conversations WHERE id = ?1)",
                [id],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(Error::ConversationNotFound { id: id.to_string() });
            }

            let mut statement = transaction.prepare(
                "SELECT id FROM conversation_turns
                 WHERE conversation_id = ?1 AND status = 'running'",
            )?;
            let running_turns = statement
                .query_map([id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            transaction.execute(
                "UPDATE conversation_turns
                 SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                     error_message = 'Conversation deleted'
                 WHERE conversation_id = ?1 AND status IN ('queued', 'running')",
                [id],
            )?;
            for turn_id in &running_turns {
                before_delete(turn_id);
            }
            transaction.execute(
                "UPDATE tasks SET conversation_id = NULL, updated_at = CURRENT_TIMESTAMP
                 WHERE conversation_id = ?1",
                [id],
            )?;
            transaction.execute("DELETE FROM conversations WHERE id = ?1", [id])?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn update(
        &self,
        id: &str,
        title: Option<&str>,
        icon: Option<&str>,
    ) -> Result<Conversation> {
        self.db.with_conn(|conn| {
            if let Some(t) = title {
                conn.execute(
                    "UPDATE conversations SET title = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
                    rusqlite::params![t, id],
                )?;
            }
            if let Some(i) = icon {
                conn.execute(
                    "UPDATE conversations SET icon = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
                    rusqlite::params![i, id],
                )?;
            }
            Ok::<(), Error>(())
        })?;
        self.get(id)
    }

    pub fn add_participant(
        &self,
        conv_id: &str,
        participant_type: &str,
        participant_id: &str,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            // Reserve the write transaction before validating ownership so an
            // Agent cannot move Projects between this check and insertion.
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let conversation_project = transaction
                .query_row(
                    "SELECT project_id FROM conversations WHERE id = ?1",
                    [conv_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|_| Error::ConversationNotFound {
                    id: conv_id.to_string(),
                })?;
            if let Some(project_id) = conversation_project.as_deref() {
                ensure_project_accepts_work(&transaction, project_id)?;
            }

            if participant_type == "agent" {
                let agent_project = transaction
                    .query_row(
                    "SELECT project_id FROM agents WHERE id = ?1",
                    [participant_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(|_| Error::AgentNotFound {
                    name: participant_id.to_string(),
                })?;
                if conversation_project.is_some() && agent_project != conversation_project {
                    return Err(Error::Conversation(
                        "an Agent must belong to the conversation's project before it can join"
                            .into(),
                    ));
                }
                if agent_project.as_deref() != conversation_project.as_deref() {
                    if let Some(project_id) = agent_project.as_deref() {
                        ensure_project_accepts_work(&transaction, project_id)?;
                    }
                }
            }

            transaction.execute(
                "INSERT OR IGNORE INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, ?2, ?3)",
                rusqlite::params![conv_id, participant_type, participant_id],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn remove_participant(
        &self,
        conv_id: &str,
        participant_type: &str,
        participant_id: &str,
    ) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let running_turns = if participant_type == "agent" {
                let mut statement = transaction.prepare(
                    "SELECT id FROM conversation_turns
                     WHERE conversation_id = ?1 AND agent_id = ?2 AND status = 'running'",
                )?;
                let ids = statement
                    .query_map(rusqlite::params![conv_id, participant_id], |row| row.get(0))?
                    .collect::<std::result::Result<Vec<String>, _>>()?;
                drop(statement);
                ids
            } else {
                Vec::new()
            };
            transaction.execute(
                "DELETE FROM conversation_participants WHERE conversation_id = ?1 AND participant_type = ?2 AND participant_id = ?3",
                rusqlite::params![conv_id, participant_type, participant_id],
            )?;
            if participant_type == "agent" {
                transaction.execute(
                    "UPDATE conversation_turns
                     SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                         error_message = 'Agent removed from conversation'
                     WHERE conversation_id = ?1 AND agent_id = ?2
                       AND status IN ('queued', 'running')",
                    rusqlite::params![conv_id, participant_id],
                )?;
                transaction.execute(
                    "UPDATE conversation_agent_sessions
                     SET status = 'idle', last_error = NULL, updated_at = CURRENT_TIMESTAMP
                     WHERE conversation_id = ?1 AND agent_id = ?2",
                    rusqlite::params![conv_id, participant_id],
                )?;
                transaction.execute(
                    "DELETE FROM schedules
                     WHERE conversation_id = ?1 AND agent_id = ?2",
                    rusqlite::params![conv_id, participant_id],
                )?;
            }
            transaction.commit()?;
            Ok(running_turns)
        })
    }

    pub fn send_message(&self, conv_id: &str, msg: &SendMessage) -> Result<ConversationMessage> {
        self.send_structured_message(conv_id, msg, None, None)
    }

    /// Create, dispatch, and publish one linked task as a single Conversation
    /// lifecycle operation.
    ///
    /// Holding the immediate transaction through message insertion means
    /// participant or Conversation deletion linearizes entirely before this
    /// call (and rejects it) or after the task and its publication are durable.
    pub fn create_linked_task_and_message(
        &self,
        conv_id: &str,
        task: &CreateTask,
        creator_agent_id: Option<&str>,
        message: &SendMessage,
    ) -> Result<(Task, ConversationMessage)> {
        if task.conversation_id.as_deref() != Some(conv_id) {
            return Err(Error::Conversation(
                "linked task must belong to the publishing conversation".into(),
            ));
        }
        let (task_id, message) = self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let task_id = TaskBoard::create_in_transaction(&transaction, task, creator_agent_id)?;
            if let Some(agent_id) = task.agent_id.as_deref() {
                TaskQueue::enqueue_in_transaction(&transaction, &task_id, agent_id)?;
            }
            let (message, _) = Self::insert_structured_message(
                &transaction,
                conv_id,
                message,
                Some(&task_id),
                None,
                &[],
            )?;
            transaction.commit()?;
            Ok::<_, Error>((task_id, message))
        })?;
        Ok((TaskBoard::new(self.db.clone()).get(&task_id)?, message))
    }

    pub fn send_structured_message(
        &self,
        conv_id: &str,
        msg: &SendMessage,
        linked_task_id: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> Result<ConversationMessage> {
        self.send_structured_message_with_attachments(conv_id, msg, linked_task_id, metadata, &[])
            .map(|(message, _)| message)
    }

    pub fn send_structured_message_with_attachments(
        &self,
        conv_id: &str,
        msg: &SendMessage,
        linked_task_id: Option<&str>,
        metadata: Option<&serde_json::Value>,
        attachments: &[NewConversationAttachment],
    ) -> Result<(ConversationMessage, Vec<ConversationAttachment>)> {
        validate_new_attachments(attachments)?;
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let result = Self::insert_structured_message(
                &transaction,
                conv_id,
                msg,
                linked_task_id,
                metadata,
                attachments,
            )?;
            transaction.commit()?;
            Ok(result)
        })
    }

    /// Store a message and schedule every addressed Agent atomically.
    pub fn send_routed_message_with_attachments(
        &self,
        conv_id: &str,
        msg: &SendMessage,
        linked_task_id: Option<&str>,
        metadata: Option<&serde_json::Value>,
        attachments: &[NewConversationAttachment],
    ) -> Result<(
        ConversationMessage,
        Vec<ConversationAttachment>,
        Vec<String>,
    )> {
        validate_new_attachments(attachments)?;
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let (message, attachments) = Self::insert_structured_message(
                &transaction,
                conv_id,
                msg,
                linked_task_id,
                metadata,
                attachments,
            )?;
            let queued_agents = runtime::ConversationTurnQueue::enqueue_for_message_in_transaction(
                &transaction,
                conv_id,
                message.id,
                &msg.sender_type,
                &msg.sender_id,
                &msg.content,
            )?;
            transaction.commit()?;
            Ok((message, attachments, queued_agents))
        })
    }

    /// Store an Agent-authored message only while its Conversation membership
    /// and optional source task still belong to the same routing scope.
    pub fn send_agent_routed_message_with_attachments(
        &self,
        conv_id: &str,
        msg: &SendMessage,
        source_task_id: Option<&str>,
        attachments: &[NewConversationAttachment],
    ) -> Result<(
        ConversationMessage,
        Vec<ConversationAttachment>,
        Vec<String>,
    )> {
        self.send_agent_routed_message_with_visualizations(
            conv_id,
            msg,
            source_task_id,
            attachments,
            None,
            &[],
        )
    }

    /// Publish a final Agent message and its already-copied visualization
    /// fragments atomically with membership validation and peer routing.
    pub fn send_agent_routed_message_with_visualizations(
        &self,
        conv_id: &str,
        msg: &SendMessage,
        source_task_id: Option<&str>,
        attachments: &[NewConversationAttachment],
        attempt_id: Option<&str>,
        visualizations: &[PreparedVisualization],
    ) -> Result<(
        ConversationMessage,
        Vec<ConversationAttachment>,
        Vec<String>,
    )> {
        validate_new_attachments(attachments)?;
        if msg.sender_type != "agent" {
            return Err(Error::Conversation(
                "Agent message validation requires an Agent sender".into(),
            ));
        }
        self.db.with_conn(|conn| {
            // Membership removal and source-task reassignment must not be able
            // to interleave between validation, publication, and peer routing.
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let is_participant = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM conversation_participants
                    WHERE conversation_id = ?1
                      AND participant_type = 'agent'
                      AND participant_id = ?2
                )",
                rusqlite::params![conv_id, msg.sender_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !is_participant {
                return Err(Error::Conversation(
                    "Agent is not a participant in this conversation".into(),
                ));
            }

            if let Some(source_task_id) = source_task_id {
                let task_scope = transaction
                    .query_row(
                        "SELECT agent_id, conversation_id FROM tasks WHERE id = ?1",
                        [source_task_id],
                        |row| {
                            Ok((
                                row.get::<_, Option<String>>(0)?,
                                row.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((agent_id, conversation_id)) = task_scope else {
                    return Err(Error::TaskNotFound {
                        id: source_task_id.to_string(),
                    });
                };
                if agent_id.as_deref() != Some(msg.sender_id.as_str())
                    || conversation_id.as_deref() != Some(conv_id)
                {
                    return Err(Error::Conversation(
                        "source task is not linked to this Agent and conversation".into(),
                    ));
                }
            }

            let (message, attachments) = Self::insert_structured_message(
                &transaction,
                conv_id,
                msg,
                source_task_id,
                None,
                attachments,
            )?;
            store_conversation_message_visualizations(
                &transaction,
                message.id,
                attempt_id,
                None,
                visualizations,
            )?;
            let queued_agents = runtime::ConversationTurnQueue::enqueue_for_message_in_transaction(
                &transaction,
                conv_id,
                message.id,
                &msg.sender_type,
                &msg.sender_id,
                &msg.content,
            )?;
            transaction.commit()?;
            Ok((message, attachments, queued_agents))
        })
    }

    /// Store a message addressed to one Agent while validating the target's
    /// membership and routing the durable message in the same write
    /// transaction. The durable routing target is stored in message metadata
    /// so a wake-up arriving behind a running turn is discovered by the same
    /// high-water routing logic after that turn completes.
    pub fn send_targeted_routed_message_with_attachments(
        &self,
        conv_id: &str,
        target_agent_id: &str,
        msg: &SendMessage,
        metadata: Option<&serde_json::Value>,
        attachments: &[NewConversationAttachment],
    ) -> Result<(
        ConversationMessage,
        Vec<ConversationAttachment>,
        Vec<String>,
    )> {
        validate_new_attachments(attachments)?;
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let is_participant = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM conversation_participants
                    WHERE conversation_id = ?1
                      AND participant_type = 'agent'
                      AND participant_id = ?2
                )",
                rusqlite::params![conv_id, target_agent_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !is_participant {
                return Err(Error::Conversation(
                    "target Agent is not a participant in this conversation".into(),
                ));
            }
            let mut routed_metadata = metadata.cloned().unwrap_or_else(|| serde_json::json!({}));
            let Some(routed_metadata) = routed_metadata.as_object_mut() else {
                return Err(Error::Conversation(
                    "targeted message metadata must be an object".into(),
                ));
            };
            routed_metadata.insert(
                "target_agent_id".to_string(),
                serde_json::Value::String(target_agent_id.to_string()),
            );
            let routed_metadata = serde_json::Value::Object(routed_metadata.clone());
            let (message, attachments) = Self::insert_structured_message(
                &transaction,
                conv_id,
                msg,
                None,
                Some(&routed_metadata),
                attachments,
            )?;
            let queued_agents = if runtime::ConversationTurnQueue::enqueue_target_in_transaction(
                &transaction,
                conv_id,
                target_agent_id,
                message.id,
            )? {
                vec![target_agent_id.to_string()]
            } else {
                Vec::new()
            };
            transaction.commit()?;
            Ok((message, attachments, queued_agents))
        })
    }

    pub(crate) fn insert_structured_message(
        transaction: &rusqlite::Transaction<'_>,
        conv_id: &str,
        msg: &SendMessage,
        linked_task_id: Option<&str>,
        metadata: Option<&serde_json::Value>,
        attachments: &[NewConversationAttachment],
    ) -> Result<(ConversationMessage, Vec<ConversationAttachment>)> {
        let conversation_project = transaction
            .query_row(
                "SELECT project_id FROM conversations WHERE id = ?1",
                [conv_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or_else(|| Error::ConversationNotFound {
                id: conv_id.to_string(),
            })?;
        if let Some(project_id) = conversation_project.as_deref() {
            ensure_project_accepts_work(transaction, project_id)?;
        }
        let message_type = msg.message_type.as_deref().unwrap_or("message");
        let metadata = metadata.cloned().unwrap_or_else(|| serde_json::json!({}));
        transaction.execute(
            "INSERT INTO conversation_messages
             (conversation_id, sender_type, sender_id, sender_name, content, message_type, linked_task_id, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                conv_id,
                msg.sender_type,
                msg.sender_id,
                msg.sender_name,
                msg.content,
                message_type,
                linked_task_id,
                metadata.to_string(),
            ],
        )?;

        let id = transaction.last_insert_rowid();
        let mut inserted_attachments = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let attachment_id = Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT INTO conversation_message_attachments
                 (id, message_id, name, mime_type, data, size, source_task_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    attachment_id,
                    id,
                    attachment.name,
                    attachment.mime_type,
                    attachment.data,
                    attachment.data.len() as i64,
                    attachment.source_task_id,
                ],
            )?;
            inserted_attachments.push(read_attachment(transaction, &attachment_id)?);
        }

        transaction.execute(
            "UPDATE conversations
             SET last_message_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1",
            [conv_id],
        )?;
        let message = transaction
            .query_row(
                "SELECT * FROM conversation_messages WHERE id = ?1",
                [id],
                row_to_message,
            )
            .map_err(|error| Error::Database(error.to_string()))?;
        Ok((message, inserted_attachments))
    }

    pub fn add_attachment(
        &self,
        message_id: i64,
        name: &str,
        mime_type: &str,
        data: &[u8],
        source_task_id: Option<&str>,
    ) -> Result<ConversationAttachment> {
        const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
        if data.len() > MAX_ATTACHMENT_BYTES {
            return Err(Error::Conversation(
                "conversation attachments must be 20 MiB or smaller".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversation_message_attachments
                 (id, message_id, name, mime_type, data, size, source_task_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    id,
                    message_id,
                    name,
                    mime_type,
                    data,
                    data.len() as i64,
                    source_task_id,
                ],
            )?;
            read_attachment(conn, &id)
        })
    }

    pub fn attachments(&self, message_id: i64) -> Result<Vec<ConversationAttachment>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, message_id, name, mime_type, size, source_task_id, created_at
                 FROM conversation_message_attachments
                 WHERE message_id = ?1 ORDER BY created_at, id",
            )?;
            let attachments = statement
                .query_map([message_id], row_to_attachment)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(attachments)
        })
    }

    pub fn visualizations(&self, message_id: i64) -> Result<Vec<MessageVisualization>> {
        VisualizationManager::new(self.db.clone()).list_for_conversation_message(message_id)
    }

    pub fn attachment_data(
        &self,
        conversation_id: &str,
        attachment_id: &str,
    ) -> Result<(ConversationAttachment, Vec<u8>)> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT a.id, a.message_id, a.name, a.mime_type, a.size,
                        a.source_task_id, a.created_at, a.data
                 FROM conversation_message_attachments a
                 JOIN conversation_messages m ON m.id = a.message_id
                 WHERE a.id = ?1 AND m.conversation_id = ?2",
                rusqlite::params![attachment_id, conversation_id],
                |row| Ok((row_to_attachment(row)?, row.get(7)?)),
            )
            .map_err(|_| Error::Conversation("attachment not found".into()))
        })
    }

    pub fn get_messages(
        &self,
        conv_id: &str,
        limit: i64,
        before_id: Option<i64>,
    ) -> Result<Vec<ConversationMessage>> {
        self.db.with_conn(|conn| {
            if let Some(bid) = before_id {
                let mut stmt = conn.prepare(
                    "SELECT * FROM conversation_messages
                     WHERE conversation_id = ?1 AND id < ?2 AND deleted_at IS NULL
                     ORDER BY id DESC LIMIT ?3",
                )?;
                let mut msgs: Vec<ConversationMessage> = stmt
                    .query_map(rusqlite::params![conv_id, bid, limit], row_to_message)?
                    .filter_map(|r| r.ok())
                    .collect();
                msgs.reverse();
                Ok(msgs)
            } else {
                let mut stmt = conn.prepare(
                    "SELECT * FROM (
                        SELECT * FROM conversation_messages
                        WHERE conversation_id = ?1 AND deleted_at IS NULL
                        ORDER BY id DESC LIMIT ?2
                     ) ORDER BY id ASC",
                )?;
                let msgs: Vec<ConversationMessage> = stmt
                    .query_map(rusqlite::params![conv_id, limit], row_to_message)?
                    .filter_map(|r| r.ok())
                    .collect();
                Ok(msgs)
            }
        })
    }

    /// Return durable message tombstones so clients can reconcile deletions
    /// that happened while their live event stream was disconnected.
    pub fn deleted_message_ids(&self, conv_id: &str) -> Result<Vec<i64>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id FROM conversation_messages
                 WHERE conversation_id = ?1 AND deleted_at IS NOT NULL
                 ORDER BY id ASC",
            )?;
            let ids = statement
                .query_map([conv_id], |row| row.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(ids)
        })
    }

    /// Hide a message, remove its local artifacts, and cancel every response
    /// that the message caused. The row itself remains as a synchronized
    /// tombstone so an older Project checkout cannot make it reappear.
    pub fn delete_message(&self, conv_id: &str, message_id: i64) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let exists = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM conversation_messages
                    WHERE id = ?1 AND conversation_id = ?2 AND deleted_at IS NULL
                 )",
                rusqlite::params![message_id, conv_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(Error::Conversation(format!(
                    "message {message_id} not found in conversation {conv_id}"
                )));
            }

            let running_turns =
                runtime::reconcile_message_deletion_turns(&transaction, conv_id, message_id)?;
            transaction.execute(
                "DELETE FROM conversation_message_attachments WHERE message_id = ?1",
                [message_id],
            )?;
            transaction.execute(
                "DELETE FROM message_visualizations WHERE conversation_message_id = ?1",
                [message_id],
            )?;
            transaction.execute(
                "UPDATE conversation_messages
                 SET deleted_at = CURRENT_TIMESTAMP, processed = 1
                 WHERE id = ?1 AND conversation_id = ?2 AND deleted_at IS NULL",
                rusqlite::params![message_id, conv_id],
            )?;
            transaction.execute(
                "UPDATE conversations
                 SET last_message_at = (
                         SELECT created_at FROM conversation_messages
                         WHERE conversation_id = ?1 AND deleted_at IS NULL
                         ORDER BY julianday(created_at) DESC, id DESC LIMIT 1
                     ),
                     updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                [conv_id],
            )?;
            transaction.commit()?;
            Ok(running_turns)
        })
    }

    /// Extract @[AGENT:id:name] mentions from content.
    pub fn parse_mentions(content: &str) -> Vec<(String, String, String)> {
        let mut mentions = Vec::new();
        let mut start = 0;
        while let Some(pos) = content[start..].find("@[") {
            let abs_pos = start + pos;
            if let Some(end) = content[abs_pos..].find(']') {
                let inner = &content[abs_pos + 2..abs_pos + end];
                let parts: Vec<&str> = inner.splitn(3, ':').collect();
                if parts.len() == 3 {
                    mentions.push((
                        parts[0].to_string(), // type: AGENT, USER
                        parts[1].to_string(), // id
                        parts[2].to_string(), // display name
                    ));
                }
                start = abs_pos + end + 1;
            } else {
                break;
            }
        }
        mentions
    }

    /// Get agent IDs mentioned in content, or all agent participants if none explicitly mentioned.
    pub fn resolve_target_agents(&self, conv_id: &str, content: &str) -> Result<Vec<String>> {
        let participants = self
            .db
            .with_conn(|conn| self.load_participants(conn, conv_id))?;
        let participant_agents = participants
            .iter()
            .filter(|participant| participant.participant_type == "agent")
            .map(|participant| participant.participant_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mentions = Self::parse_mentions(content);
        let mut mentioned_agents: Vec<String> = mentions
            .iter()
            .filter(|(t, _, _)| t == "AGENT")
            .map(|(_, id, _)| id.clone())
            .filter(|id| participant_agents.contains(id.as_str()))
            .collect();
        mentioned_agents.sort();
        mentioned_agents.dedup();

        if !mentioned_agents.is_empty() {
            return Ok(mentioned_agents);
        }
        if mentions.iter().any(|(kind, _, _)| kind == "AGENT") {
            return Ok(Vec::new());
        }

        // No explicit mention — auto-route to all agent participants
        Ok(participants
            .iter()
            .filter(|p| p.participant_type == "agent")
            .map(|p| p.participant_id.clone())
            .collect())
    }

    fn load_participants(
        &self,
        conn: &rusqlite::Connection,
        conv_id: &str,
    ) -> Result<Vec<Participant>> {
        let mut stmt = conn.prepare(
            "SELECT participant_type, participant_id, joined_at
             FROM conversation_participants WHERE conversation_id = ?1",
        )?;

        let participants = stmt
            .query_map([conv_id], |row| {
                Ok(Participant {
                    participant_type: row.get("participant_type")?,
                    participant_id: row.get("participant_id")?,
                    joined_at: row.get("joined_at")?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(participants)
    }

    // -- Background processing methods (ADR-019) --

    /// Compatibility entry point used by connectors and the retired task
    /// dispatcher. New conversation turns use the durable ACP queue rather
    /// than the legacy `processed` flag.
    pub fn send_user_message(
        &self,
        conv_id: &str,
        msg: &SendMessage,
    ) -> Result<ConversationMessage> {
        let (message, _, _) =
            self.send_routed_message_with_attachments(conv_id, msg, None, None, &[])?;
        Ok(message)
    }

    /// Check if there are unprocessed user messages in a conversation.
    pub fn has_unprocessed(&self, conv_id: &str) -> bool {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_messages
                     WHERE conversation_id = ?1 AND sender_type = 'user'
                       AND processed = 0 AND deleted_at IS NULL",
                    [conv_id],
                    |row| row.get::<_, i64>(0),
                )
            })
            .unwrap_or(0)
            > 0
    }

    /// Mark all unprocessed user messages as processed.
    pub fn mark_processed(&self, conv_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE conversation_messages SET processed = 1
                 WHERE conversation_id = ?1 AND sender_type = 'user' AND processed = 0",
                [conv_id],
            )
        })?;
        Ok(())
    }

    /// Get messages after a given message ID (for SSE replay).
    pub fn get_messages_after(
        &self,
        conv_id: &str,
        after_id: i64,
    ) -> Result<Vec<ConversationMessage>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT * FROM conversation_messages
                 WHERE conversation_id = ?1 AND id > ?2 AND deleted_at IS NULL
                 ORDER BY id ASC",
            )?;
            let msgs = stmt
                .query_map(rusqlite::params![conv_id, after_id], row_to_message)?
                .filter_map(|r| r.ok())
                .collect();
            Ok(msgs)
        })
    }

    /// Return the newest messages in an inclusive high-water range. This is
    /// used when resuming an Agent's durable conversation session so prior
    /// history is not resent on every turn.
    pub fn get_messages_between(
        &self,
        conv_id: &str,
        after_id: i64,
        through_id: i64,
        limit: i64,
    ) -> Result<Vec<ConversationMessage>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT * FROM (
                    SELECT * FROM conversation_messages
                    WHERE conversation_id = ?1 AND id > ?2 AND id <= ?3
                      AND deleted_at IS NULL
                    ORDER BY id DESC LIMIT ?4
                 ) ORDER BY id ASC",
            )?;
            let messages = stmt
                .query_map(
                    rusqlite::params![conv_id, after_id, through_id, limit],
                    row_to_message,
                )?
                .filter_map(|result| result.ok())
                .collect();
            Ok(messages)
        })
    }

    /// Set the processing status of a conversation.
    pub fn set_processing_status(&self, conv_id: &str, status: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE conversations SET processing_status = ?1 WHERE id = ?2",
                rusqlite::params![status, conv_id],
            )
        })?;
        Ok(())
    }

    /// Check if a conversation is currently being processed.
    pub fn is_processing(&self, conv_id: &str) -> bool {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT processing_status FROM conversations WHERE id = ?1",
                    [conv_id],
                    |row| row.get::<_, String>(0),
                )
            })
            .unwrap_or_else(|_| "idle".to_string())
            == "processing"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manager() -> ConversationManager {
        let db = Arc::new(Database::open_memory().unwrap());
        // Register a test agent
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agents (id, name, backend, config) VALUES ('atlas', 'atlas', 'generic', '{}')",
                [],
            ).unwrap();
        });
        ConversationManager::new(db)
    }

    #[test]
    fn test_create_and_get() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Test Chat".into()),
                icon: Some("💬".into()),
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();

        assert_eq!(conv.title, Some("Test Chat".into()));
        assert_eq!(conv.icon, Some("💬".into()));
        // user + agent = 2 participants
        assert_eq!(conv.participants.len(), 2);

        let got = mgr.get(&conv.id).unwrap();
        assert_eq!(got.id, conv.id);
    }

    #[test]
    fn test_list_ordered_by_activity() {
        let mgr = test_manager();
        let c1 = mgr
            .create(&CreateConversation {
                title: Some("First".into()),
                icon: None,
                participant_ids: vec![],
            })
            .unwrap();
        let c2 = mgr
            .create(&CreateConversation {
                title: Some("Second".into()),
                icon: None,
                participant_ids: vec![],
            })
            .unwrap();

        // Send message to first conv so it becomes most recent
        mgr.send_message(
            &c1.id,
            &SendMessage {
                sender_type: "user".into(),
                sender_id: "local".into(),
                sender_name: Some("User".into()),
                content: "hello".into(),
                message_type: None,
            },
        )
        .unwrap();

        let list = mgr.list(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, c1.id); // c1 has message, so it's first
        assert_eq!(list[1].id, c2.id);
    }

    #[test]
    fn test_delete() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: None,
                icon: None,
                participant_ids: vec![],
            })
            .unwrap();

        mgr.delete(&conv.id).unwrap();
        assert!(mgr.get(&conv.id).is_err());
    }

    #[test]
    fn deleting_a_conversation_cancels_and_reports_running_turns_first() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Delete me".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let board = crate::tasks::board::TaskBoard::new(mgr.db.clone());
        let linked_task = board
            .create(&crate::tasks::board::CreateTask {
                title: "Keep the task".into(),
                agent_id: Some("atlas".into()),
                conversation_id: Some(conv.id.clone()),
                ..Default::default()
            })
            .unwrap();
        mgr.db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO conversation_turns
                     (id, conversation_id, agent_id, status, started_at)
                     VALUES ('running-turn', ?1, 'atlas', 'running', CURRENT_TIMESTAMP)",
                    [&conv.id],
                )?;
                Ok::<(), Error>(())
            })
            .unwrap();

        let mut interrupted = Vec::new();
        mgr.delete_with_running_turns(&conv.id, |turn_id| interrupted.push(turn_id.to_string()))
            .unwrap();

        assert_eq!(interrupted, ["running-turn"]);
        assert!(mgr.get(&conv.id).is_err());
        let remaining = mgr
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_turns WHERE conversation_id = ?1",
                    [&conv.id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(Error::from)
            })
            .unwrap();
        assert_eq!(remaining, 0);
        assert_eq!(
            board
                .get(&linked_task.id)
                .unwrap()
                .conversation_id
                .as_deref(),
            None
        );
    }

    #[test]
    fn test_messages() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: None,
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();

        let m1 = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("Eduardo".into()),
                    content: "Hello @[AGENT:atlas:atlas]".into(),
                    message_type: None,
                },
            )
            .unwrap();

        assert_eq!(m1.sender_type, "user");
        assert_eq!(m1.message_type, "message");

        let m2 = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: Some("atlas".into()),
                    content: "Hello! How can I help?".into(),
                    message_type: None,
                },
            )
            .unwrap();

        let msgs = mgr.get_messages(&conv.id, 50, None).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].id, m1.id);
        assert_eq!(msgs[1].id, m2.id);
    }

    #[test]
    fn deleting_a_message_cancels_its_response_and_leaves_a_sync_tombstone() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Delete one message".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let first = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Keep this".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let deleted = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Delete this".into(),
                    message_type: None,
                },
            )
            .unwrap();
        mgr.add_attachment(deleted.id, "note.txt", "text/plain", b"secret", None)
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        queue.enqueue(&conv.id, "atlas", deleted.id).unwrap();
        let running = queue.claim_next().unwrap().unwrap();

        let interrupted = mgr.delete_message(&conv.id, deleted.id).unwrap();

        assert_eq!(interrupted.as_slice(), std::slice::from_ref(&running.id));
        let visible = mgr.get_messages(&conv.id, 50, None).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, first.id);
        assert!(mgr.attachments(deleted.id).unwrap().is_empty());
        assert!(queue
            .list_for_conversation(&conv.id, 10)
            .unwrap()
            .iter()
            .any(|turn| turn.status == "queued" && turn.trigger_message_id == Some(first.id)));
        let (deleted_at, turn_status, session_status, last_message_at, dashboard_event_kind) = mgr
            .db
            .with_conn(|conn| {
                Ok::<_, Error>((
                    conn.query_row(
                        "SELECT deleted_at FROM conversation_messages WHERE id = ?1",
                        [deleted.id],
                        |row| row.get::<_, Option<String>>(0),
                    )?,
                    conn.query_row(
                        "SELECT status FROM conversation_turns WHERE id = ?1",
                        [&running.id],
                        |row| row.get::<_, String>(0),
                    )?,
                    conn.query_row(
                        "SELECT status FROM conversation_agent_sessions
                         WHERE conversation_id = ?1 AND agent_id = 'atlas'",
                        [&conv.id],
                        |row| row.get::<_, String>(0),
                    )?,
                    conn.query_row(
                        "SELECT last_message_at FROM conversations WHERE id = ?1",
                        [&conv.id],
                        |row| row.get::<_, Option<String>>(0),
                    )?,
                    conn.query_row(
                        "SELECT event_kind FROM dashboard_events
                         WHERE event_id = 'conversation-message:' || ?1
                         ORDER BY cursor DESC LIMIT 1",
                        [deleted.id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?,
                ))
            })
            .unwrap();
        assert!(deleted_at.is_some());
        assert_eq!(turn_status, "cancelled");
        assert_eq!(session_status, "queued");
        assert_eq!(last_message_at.as_deref(), Some(first.created_at.as_str()));
        assert_eq!(
            dashboard_event_kind.as_deref(),
            Some("conversation_message_deleted")
        );
        assert_eq!(mgr.deleted_message_ids(&conv.id).unwrap(), vec![deleted.id]);
    }

    #[test]
    fn deleting_a_coalesced_message_keeps_the_earlier_queued_response() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Retarget queued work".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let first = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Keep this request".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let second = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Delete only this follow-up".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        assert!(queue.enqueue(&conv.id, "atlas", first.id).unwrap());
        assert!(!queue.enqueue(&conv.id, "atlas", second.id).unwrap());

        let interrupted = mgr.delete_message(&conv.id, second.id).unwrap();

        assert!(interrupted.is_empty());
        let turns = queue.list_for_conversation(&conv.id, 10).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, "queued");
        assert_eq!(turns[0].trigger_message_id, Some(first.id));
        assert_eq!(
            turns[0].response_queued_at.as_deref(),
            Some(first.created_at.as_str())
        );
        assert_eq!(mgr.get_messages(&conv.id, 10, None).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_running_coalesced_message_requeues_visible_work() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Retain work absorbed by a running turn".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let first = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Keep this request".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let deleted = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Delete this coalesced follow-up".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        assert!(queue.enqueue(&conv.id, "atlas", first.id).unwrap());
        assert!(!queue.enqueue(&conv.id, "atlas", deleted.id).unwrap());
        let running = queue.claim_next().unwrap().unwrap();
        assert_eq!(running.trigger_message_id, Some(deleted.id));
        mgr.db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE conversation_agent_sessions SET native_session_id = 'tainted-session'
                     WHERE conversation_id = ?1 AND agent_id = 'atlas'",
                    [&conv.id],
                )
            })
            .unwrap();

        let interrupted = mgr.delete_message(&conv.id, deleted.id).unwrap();

        assert_eq!(interrupted, vec![running.id.clone()]);
        let turns = queue.list_for_conversation(&conv.id, 10).unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns
            .iter()
            .any(|turn| turn.id == running.id && turn.status == "cancelled"));
        assert!(turns
            .iter()
            .any(|turn| { turn.status == "queued" && turn.trigger_message_id == Some(first.id) }));
        assert_eq!(
            queue
                .last_terminal_trigger_before(&conv.id, "atlas", first.id)
                .unwrap(),
            None
        );
        assert!(queue
            .session(&conv.id, "atlas")
            .unwrap()
            .native_session_id
            .is_none());
    }

    #[test]
    fn deleting_an_earlier_message_cancels_a_running_coalesced_turn() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Remove an earlier coalesced prompt".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let deleted = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Delete this first request".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let retained = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Keep this coalesced follow-up".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        assert!(queue.enqueue(&conv.id, "atlas", deleted.id).unwrap());
        assert!(!queue.enqueue(&conv.id, "atlas", retained.id).unwrap());
        let running = queue.claim_next().unwrap().unwrap();
        assert_eq!(running.trigger_message_id, Some(retained.id));

        let interrupted = mgr.delete_message(&conv.id, deleted.id).unwrap();

        assert_eq!(interrupted, vec![running.id.clone()]);
        let turns = queue.list_for_conversation(&conv.id, 10).unwrap();
        assert!(turns
            .iter()
            .any(|turn| turn.id == running.id && turn.status == "cancelled"));
        assert!(turns.iter().any(|turn| {
            turn.status == "queued" && turn.trigger_message_id == Some(retained.id)
        }));
        assert_eq!(
            mgr.get_messages(&conv.id, 10, None).unwrap()[0].id,
            retained.id
        );
    }

    #[test]
    fn deleting_an_earlier_message_dismisses_a_failed_coalesced_turn() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Dismiss a failed coalesced prompt".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let deleted = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Delete this failed request".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let retained = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Retain this coalesced context".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        assert!(queue.enqueue(&conv.id, "atlas", deleted.id).unwrap());
        assert!(!queue.enqueue(&conv.id, "atlas", retained.id).unwrap());
        let failed = queue.claim_next().unwrap().unwrap();
        assert_eq!(failed.trigger_message_id, Some(retained.id));
        queue.fail(&failed, "agent refused the prompt").unwrap();

        let interrupted = mgr.delete_message(&conv.id, deleted.id).unwrap();

        assert!(interrupted.is_empty());
        let turns = queue.list_for_conversation(&conv.id, 10).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].id, failed.id);
        assert_eq!(turns[0].status, "cancelled");
        assert_eq!(turns[0].trigger_message_id, Some(deleted.id));
        let session: (String, Option<String>) = mgr
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT status, last_error FROM conversation_agent_sessions
                     WHERE conversation_id = ?1 AND agent_id = 'atlas'",
                    [&conv.id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(session, ("idle".into(), None));
        let attention = mgr
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM dashboard_events
                     WHERE target_type = 'conversation' AND target_id = ?1
                       AND needs_attention = 1",
                    [&conv.id],
                    |row| row.get::<_, i64>(0),
                )
            })
            .unwrap();
        assert_eq!(attention, 0);

        let later = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "New work after dismissal".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let boundary = queue
            .last_interrupted_trigger_before(&conv.id, "atlas", later.id)
            .unwrap();
        assert_eq!(boundary, Some(deleted.id));
        let reconstructed = mgr
            .get_messages_between(&conv.id, boundary.unwrap(), later.id, 80)
            .unwrap();
        assert_eq!(
            reconstructed
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["Retain this coalesced context", "New work after dismissal"]
        );
    }

    #[test]
    fn deleting_a_completed_prompt_resets_the_resumable_session() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Reset completed prompt history".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let prompt = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Remove this completed prompt".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        assert!(queue.enqueue(&conv.id, "atlas", prompt.id).unwrap());
        let running = queue.claim_next().unwrap().unwrap();
        let result = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: Some("Atlas".into()),
                    content: "Completed response".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .complete(&running, "completed-session", result.id)
            .unwrap();
        assert_eq!(
            queue
                .session(&conv.id, "atlas")
                .unwrap()
                .native_session_id
                .as_deref(),
            Some("completed-session")
        );

        let interrupted = mgr.delete_message(&conv.id, prompt.id).unwrap();

        assert!(interrupted.is_empty());
        assert!(queue
            .session(&conv.id, "atlas")
            .unwrap()
            .native_session_id
            .is_none());
    }

    #[test]
    fn cancelling_a_running_turn_queues_its_deferred_follow_up() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Keep deferred follow-up".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let first = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Stop this response".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        assert!(queue.enqueue(&conv.id, "atlas", first.id).unwrap());
        let running = queue.claim_next().unwrap().unwrap();
        mgr.db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE conversation_agent_sessions SET native_session_id = 'resumed-session'
                     WHERE conversation_id = ?1 AND agent_id = 'atlas'",
                    [&conv.id],
                )
            })
            .unwrap();
        let follow_up = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Answer this follow-up instead".into(),
                    message_type: None,
                },
            )
            .unwrap();
        assert!(!queue.enqueue(&conv.id, "atlas", follow_up.id).unwrap());

        let cancellation = queue.cancel(&conv.id, &running.id).unwrap();

        assert!(cancellation.changed);
        assert!(cancellation.was_running);
        let turns = queue.list_for_conversation(&conv.id, 10).unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns
            .iter()
            .any(|turn| turn.id == running.id && turn.status == "cancelled"));
        assert!(turns.iter().any(|turn| {
            turn.status == "queued" && turn.trigger_message_id == Some(follow_up.id)
        }));
        assert_eq!(
            queue
                .last_terminal_trigger_before(&conv.id, "atlas", follow_up.id)
                .unwrap(),
            Some(first.id)
        );
        assert!(queue
            .session(&conv.id, "atlas")
            .unwrap()
            .native_session_id
            .is_none());
    }

    #[test]
    fn deleting_a_new_message_does_not_retry_cancelled_work() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Do not retry cancelled work".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let cancelled_message = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Cancel this request".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queue = runtime::ConversationTurnQueue::new(mgr.db.clone());
        assert!(queue
            .enqueue(&conv.id, "atlas", cancelled_message.id)
            .unwrap());
        let cancelled_turn = queue.list_for_conversation(&conv.id, 10).unwrap().remove(0);
        assert!(queue.cancel(&conv.id, &cancelled_turn.id).unwrap().changed);

        let deleted_message = mgr
            .send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: Some("You".into()),
                    content: "Delete this later request".into(),
                    message_type: None,
                },
            )
            .unwrap();
        assert!(queue
            .enqueue(&conv.id, "atlas", deleted_message.id)
            .unwrap());

        let interrupted = mgr.delete_message(&conv.id, deleted_message.id).unwrap();

        assert!(interrupted.is_empty());
        let turns = queue.list_for_conversation(&conv.id, 10).unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns.iter().all(|turn| turn.status == "cancelled"));
        assert_eq!(
            turns
                .iter()
                .filter(|turn| turn.trigger_message_id == Some(cancelled_message.id))
                .count(),
            1
        );
    }

    #[test]
    fn agent_publication_requires_current_membership_and_source_task_scope() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Scoped Agent messages".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let board = crate::tasks::board::TaskBoard::new(mgr.db.clone());
        let source_task = board
            .create(&crate::tasks::board::CreateTask {
                title: "Publish findings".into(),
                agent_id: Some("atlas".into()),
                conversation_id: Some(conv.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let detached_task = board
            .create(&crate::tasks::board::CreateTask {
                title: "Private work".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let message = SendMessage {
            sender_type: "agent".into(),
            sender_id: "atlas".into(),
            sender_name: Some("Atlas".into()),
            content: "The findings are ready.".into(),
            message_type: None,
        };

        mgr.send_agent_routed_message_with_attachments(
            &conv.id,
            &message,
            Some(&source_task.id),
            &[],
        )
        .unwrap();
        let mismatched = mgr.send_agent_routed_message_with_attachments(
            &conv.id,
            &message,
            Some(&detached_task.id),
            &[],
        );
        assert!(matches!(mismatched, Err(Error::Conversation(_))));

        mgr.remove_participant(&conv.id, "agent", "atlas").unwrap();
        let removed = mgr.send_agent_routed_message_with_attachments(
            &conv.id,
            &message,
            Some(&source_task.id),
            &[],
        );
        assert!(matches!(removed, Err(Error::Conversation(_))));
        assert_eq!(mgr.get_messages(&conv.id, 50, None).unwrap().len(), 1);
    }

    #[test]
    fn linked_task_creation_rolls_back_if_publication_fails() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Atomic linked work".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        mgr.db
            .with_conn(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER fail_linked_task_publication
                     BEFORE INSERT ON conversation_messages
                     WHEN NEW.message_type = 'task'
                     BEGIN
                         SELECT RAISE(ABORT, 'forced linked publication failure');
                     END;",
                )
            })
            .unwrap();

        let task = CreateTask {
            title: "Investigate atomically".into(),
            agent_id: Some("atlas".into()),
            conversation_id: Some(conv.id.clone()),
            context: Some(serde_json::json!({ "origin": "conversation" })),
            ..Default::default()
        };
        let message = SendMessage {
            sender_type: "agent".into(),
            sender_id: "atlas".into(),
            sender_name: Some("Atlas".into()),
            content: "Created task: Investigate atomically".into(),
            message_type: Some("task".into()),
        };
        assert!(mgr
            .create_linked_task_and_message(&conv.id, &task, Some("atlas"), &message)
            .is_err());

        let rolled_back = mgr
            .db
            .with_conn(|conn| {
                Ok::<_, Error>((
                    conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get::<_, i64>(0))?,
                    conn.query_row("SELECT COUNT(*) FROM task_queue", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM work_attempts", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM conversation_messages", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                ))
            })
            .unwrap();
        assert_eq!(rolled_back, (0, 0, 0, 0));

        mgr.db
            .with_conn(|conn| conn.execute_batch("DROP TRIGGER fail_linked_task_publication"))
            .unwrap();
        let (created, published) = mgr
            .create_linked_task_and_message(&conv.id, &task, Some("atlas"), &message)
            .unwrap();
        assert_eq!(
            published.linked_task_id.as_deref(),
            Some(created.id.as_str())
        );
        assert_eq!(created.conversation_id.as_deref(), Some(conv.id.as_str()));
        let committed = mgr
            .db
            .with_conn(|conn| {
                Ok::<_, Error>((
                    conn.query_row("SELECT COUNT(*) FROM task_queue", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM work_attempts", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                ))
            })
            .unwrap();
        assert_eq!(committed, (1, 1));
    }

    #[test]
    fn test_message_pagination() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: None,
                icon: None,
                participant_ids: vec![],
            })
            .unwrap();

        for i in 0..5 {
            mgr.send_message(
                &conv.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: format!("msg {i}"),
                    message_type: None,
                },
            )
            .unwrap();
        }

        // Get last 3
        let msgs = mgr.get_messages(&conv.id, 3, None).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "msg 2");

        // Get 2 before the first of those 3
        let before = mgr.get_messages(&conv.id, 2, Some(msgs[0].id)).unwrap();
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].content, "msg 0");
        assert_eq!(before[1].content, "msg 1");

        let between = mgr
            .get_messages_between(&conv.id, before[0].id, msgs[1].id, 10)
            .unwrap();
        assert_eq!(
            between
                .iter()
                .map(|message| message.content.as_str())
                .collect::<Vec<_>>(),
            vec!["msg 1", "msg 2", "msg 3"]
        );
    }

    #[test]
    fn test_parse_mentions() {
        let mentions = ConversationManager::parse_mentions(
            "Hey @[AGENT:atlas:atlas] and @[AGENT:flynn:flynn], how are you?",
        );
        assert_eq!(mentions.len(), 2);
        assert_eq!(
            mentions[0],
            ("AGENT".into(), "atlas".into(), "atlas".into())
        );
        assert_eq!(
            mentions[1],
            ("AGENT".into(), "flynn".into(), "flynn".into())
        );

        // No mentions
        assert!(ConversationManager::parse_mentions("no mentions here").is_empty());
    }

    #[test]
    fn test_resolve_target_agents_with_mention() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: None,
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();

        let targets = mgr
            .resolve_target_agents(&conv.id, "Hey @[AGENT:atlas:atlas]")
            .unwrap();
        assert_eq!(targets, vec!["atlas"]);

        let targets = mgr
            .resolve_target_agents(&conv.id, "Hey @[AGENT:reviewer:reviewer]")
            .unwrap();
        assert!(targets.is_empty());
    }

    #[test]
    fn test_resolve_target_agents_auto_route() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: None,
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();

        // No mention — should auto-route to atlas (the only agent participant)
        let targets = mgr.resolve_target_agents(&conv.id, "hello there").unwrap();
        assert_eq!(targets, vec!["atlas"]);
    }

    #[test]
    fn test_add_remove_participant() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: None,
                icon: None,
                participant_ids: vec![],
            })
            .unwrap();

        assert_eq!(conv.participants.len(), 1); // just user

        mgr.add_participant(&conv.id, "agent", "atlas").unwrap();
        let conv = mgr.get(&conv.id).unwrap();
        assert_eq!(conv.participants.len(), 2);

        mgr.remove_participant(&conv.id, "agent", "atlas").unwrap();
        let conv = mgr.get(&conv.id).unwrap();
        assert_eq!(conv.participants.len(), 1);
    }

    #[test]
    fn removing_an_agent_cancels_its_conversation_wakeups() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Release room".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let schedules = crate::tasks::scheduler::ScheduleManager::new(mgr.db.clone());
        let wakeup = schedules
            .create_one_shot(&crate::tasks::scheduler::CreateOneShotSchedule {
                name: "Check release".into(),
                run_at: None,
                delay_seconds: Some(60),
                agent_id: "atlas".into(),
                title: "Check release".into(),
                description: Some("Update the room.".into()),
                continuation_task_id: None,
                conversation_id: Some(conv.id.clone()),
            })
            .unwrap();

        mgr.remove_participant(&conv.id, "agent", "atlas").unwrap();
        assert!(matches!(
            schedules.get(&wakeup.id),
            Err(Error::ScheduleNotFound { .. })
        ));

        mgr.add_participant(&conv.id, "agent", "atlas").unwrap();
        assert!(matches!(
            schedules.get(&wakeup.id),
            Err(Error::ScheduleNotFound { .. })
        ));
    }

    #[test]
    fn adding_an_agent_from_another_project_is_rejected_without_membership() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO projects (id, name) VALUES ('two', 'Two');
                 INSERT INTO agents (id, name, backend, config, project_id)
                    VALUES ('atlas', 'Atlas', 'native', '{}', 'two');",
            )
        })
        .unwrap();
        let manager = ConversationManager::new(db.clone());
        let conversation = manager
            .create_in_project(
                Some("one"),
                &CreateConversation {
                    title: Some("Project one".into()),
                    icon: None,
                    participant_ids: vec![],
                },
            )
            .unwrap();

        let error = manager
            .add_participant(&conversation.id, "agent", "atlas")
            .unwrap_err();
        assert!(error.to_string().contains("must belong"));
        let membership: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_participants
                     WHERE conversation_id = ?1
                       AND participant_type = 'agent'
                       AND participant_id = 'atlas'",
                    [&conversation.id],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(membership, 0);
    }

    #[test]
    fn test_update_conversation() {
        let mgr = test_manager();
        let conv = mgr
            .create(&CreateConversation {
                title: Some("Old".into()),
                icon: None,
                participant_ids: vec![],
            })
            .unwrap();

        let updated = mgr.update(&conv.id, Some("New Title"), Some("🚀")).unwrap();
        assert_eq!(updated.title, Some("New Title".into()));
        assert_eq!(updated.icon, Some("🚀".into()));
    }
}
