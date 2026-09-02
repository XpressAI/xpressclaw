use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::conversations::{row_to_message, ConversationManager, ConversationMessage, SendMessage};
use crate::dashboard::finalize_inactive_git_baselines;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::message_artifacts::PublishedFileAttachment;
use crate::projects::ensure_project_accepts_work;
use crate::visualizations::{store_conversation_message_visualizations, PreparedVisualization};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub id: String,
    pub conversation_id: String,
    pub agent_id: String,
    pub trigger_message_id: Option<i64>,
    pub status: String,
    pub result_message_id: Option<i64>,
    pub error_message: Option<String>,
    pub context_used: Option<i64>,
    pub context_size: Option<i64>,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    /// UTC timestamp of the latest message included in this response cycle.
    pub response_queued_at: Option<String>,
    /// UTC timestamp at which the agent began generating this response.
    pub response_started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationTurnCancellation {
    pub turn: ConversationTurn,
    pub changed: bool,
    pub was_running: bool,
}

#[derive(Debug, Clone)]
pub struct ConversationAgentSession {
    pub native_session_id: Option<String>,
}

pub struct ConversationTurnQueue {
    db: Arc<Database>,
}

impl ConversationTurnQueue {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Queue the participants addressed by a message. User messages address
    /// every participant when no mention is present; Agent messages only
    /// wake explicitly mentioned peers, preventing accidental reply loops.
    pub fn enqueue_for_message(
        &self,
        conversation_id: &str,
        message_id: i64,
        sender_type: &str,
        sender_id: &str,
        content: &str,
    ) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let queued = Self::enqueue_for_message_in_transaction(
                &transaction,
                conversation_id,
                message_id,
                sender_type,
                sender_id,
                content,
            )?;
            transaction.commit()?;
            Ok(queued)
        })
    }

    /// Route a message while its message row is still part of the caller's
    /// transaction. This prevents a committed Conversation message from being
    /// stranded without its addressed Agent turns after a process crash.
    pub(crate) fn enqueue_for_message_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        conversation_id: &str,
        message_id: i64,
        sender_type: &str,
        sender_id: &str,
        content: &str,
    ) -> Result<Vec<String>> {
        let agent_mentions = Self::agent_mentions(content);
        let mut statement = transaction.prepare(
            "SELECT participant_id FROM conversation_participants
             WHERE conversation_id = ?1 AND participant_type = 'agent'",
        )?;
        let participants = statement
            .query_map([conversation_id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);

        let mut targets = participants
            .into_iter()
            .filter(|agent_id| {
                Self::message_targets_agent(sender_type, sender_id, agent_id, &agent_mentions, None)
            })
            .collect::<Vec<_>>();
        targets.sort();
        targets.dedup();

        let mut queued = Vec::new();
        for agent_id in targets {
            if Self::enqueue_in_transaction(transaction, conversation_id, &agent_id, message_id)? {
                queued.push(agent_id);
            }
        }
        Ok(queued)
    }

    fn agent_mentions(content: &str) -> std::collections::HashSet<String> {
        ConversationManager::parse_mentions(content)
            .into_iter()
            .filter(|(kind, _, _)| kind == "AGENT")
            .map(|(_, id, _)| id)
            .collect()
    }

    fn message_targets_agent(
        sender_type: &str,
        sender_id: &str,
        agent_id: &str,
        agent_mentions: &std::collections::HashSet<String>,
        routed_target_agent_id: Option<&str>,
    ) -> bool {
        if agent_id == sender_id {
            return false;
        }
        if let Some(target_agent_id) = routed_target_agent_id {
            return agent_id == target_agent_id;
        }
        if agent_mentions.is_empty() {
            sender_type != "agent"
        } else {
            agent_mentions.contains(agent_id)
        }
    }

    /// Returns true when a new queued turn was inserted. A message arriving
    /// behind an existing queued turn advances that turn's high-water mark;
    /// a message arriving during a running turn is picked up on completion.
    pub fn enqueue(
        &self,
        conversation_id: &str,
        agent_id: &str,
        trigger_message_id: i64,
    ) -> Result<bool> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let inserted = Self::enqueue_in_transaction(
                &transaction,
                conversation_id,
                agent_id,
                trigger_message_id,
            )?;
            transaction.commit()?;
            Ok(inserted)
        })
    }

    fn enqueue_in_transaction(
        transaction: &rusqlite::Connection,
        conversation_id: &str,
        agent_id: &str,
        trigger_message_id: i64,
    ) -> Result<bool> {
        let project_id = transaction
            .query_row(
                "SELECT project_id FROM conversations WHERE id = ?1",
                [conversation_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or_else(|| Error::ConversationNotFound {
                id: conversation_id.to_string(),
            })?;
        if let Some(project_id) = project_id.as_deref() {
            ensure_project_accepts_work(transaction, project_id)?;
        }
        let trigger_exists = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM conversation_messages
                WHERE id = ?1 AND conversation_id = ?2 AND deleted_at IS NULL
             )",
            rusqlite::params![trigger_message_id, conversation_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !trigger_exists {
            return Err(Error::Conversation(format!(
                "message {trigger_message_id} does not belong to conversation {conversation_id}"
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
            return Ok(false);
        }

        let existing = transaction
            .query_row(
                "SELECT id, status FROM conversation_turns
                 WHERE conversation_id = ?1 AND agent_id = ?2
                   AND status IN ('queued', 'running')
                 ORDER BY queued_at DESC LIMIT 1",
                rusqlite::params![conversation_id, agent_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((id, status)) = existing {
            if status == "queued" {
                transaction.execute(
                    "UPDATE conversation_turns
                     SET response_queued_at = CASE
                            WHEN COALESCE(trigger_message_id, 0) <= ?1
                            THEN (SELECT created_at FROM conversation_messages
                                  WHERE id = ?1
                                    AND conversation_id = conversation_turns.conversation_id
                                    AND deleted_at IS NULL)
                            ELSE response_queued_at END,
                         trigger_message_id = MAX(COALESCE(trigger_message_id, 0), ?1)
                     WHERE id = ?2",
                    rusqlite::params![trigger_message_id, id],
                )?;
            }
            return Ok(false);
        }
        let id = Uuid::new_v4().to_string();
        transaction.execute(
            "INSERT INTO conversation_turns
             (id, conversation_id, agent_id, trigger_message_id, response_queued_at)
             SELECT ?1, ?2, ?3, ?4, created_at
             FROM conversation_messages
             WHERE id = ?4 AND conversation_id = ?2 AND deleted_at IS NULL",
            rusqlite::params![id, conversation_id, agent_id, trigger_message_id],
        )?;
        transaction.execute(
            "INSERT INTO conversation_agent_sessions
             (conversation_id, agent_id, status, updated_at)
             VALUES (?1, ?2, 'queued', CURRENT_TIMESTAMP)
             ON CONFLICT(conversation_id, agent_id) DO UPDATE SET
                status = 'queued', last_error = NULL, updated_at = CURRENT_TIMESTAMP",
            rusqlite::params![conversation_id, agent_id],
        )?;
        Ok(true)
    }

    pub(crate) fn enqueue_target_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        conversation_id: &str,
        agent_id: &str,
        trigger_message_id: i64,
    ) -> Result<bool> {
        Self::enqueue_in_transaction(transaction, conversation_id, agent_id, trigger_message_id)
    }

    pub fn claim_next(&self) -> Result<Option<ConversationTurn>> {
        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE conversation_turns
                 SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                     error_message = 'Agent is no longer a conversation participant'
                 WHERE status = 'queued'
                   AND NOT EXISTS (
                       SELECT 1 FROM conversation_participants p
                       WHERE p.conversation_id = conversation_turns.conversation_id
                         AND p.participant_type = 'agent'
                         AND p.participant_id = conversation_turns.agent_id
                   )",
                [],
            )?;
            let id = tx
                .query_row(
                    "SELECT t.id FROM conversation_turns t
                     JOIN conversation_participants p
                       ON p.conversation_id = t.conversation_id
                      AND p.participant_type = 'agent'
                      AND p.participant_id = t.agent_id
                     WHERE t.status = 'queued' ORDER BY t.queued_at ASC LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let Some(id) = id else {
                tx.commit()?;
                return Ok(None);
            };
            let changed = tx.execute(
                "UPDATE conversation_turns
                 SET status = 'running', started_at = CURRENT_TIMESTAMP, error_message = NULL
                 WHERE id = ?1 AND status = 'queued'",
                [&id],
            )?;
            if changed == 0 {
                tx.commit()?;
                return Ok(None);
            }
            let turn = tx.query_row(
                "SELECT * FROM conversation_turns WHERE id = ?1",
                [&id],
                row_to_turn,
            )?;
            tx.execute(
                "UPDATE conversation_agent_sessions
                 SET status = 'running', last_error = NULL, updated_at = CURRENT_TIMESTAMP
                 WHERE conversation_id = ?1 AND agent_id = ?2",
                rusqlite::params![turn.conversation_id, turn.agent_id],
            )?;
            tx.commit()?;
            Ok(Some(turn))
        })
    }

    /// Mark the true response boundary after process/session preparation and
    /// immediately before the ACP turn begins.
    pub fn start_response(&self, turn_id: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE conversation_turns
                 SET response_started_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND status = 'running' AND response_started_at IS NULL",
                [turn_id],
            )?;
            Ok(changed == 1)
        })
    }

    pub fn session(
        &self,
        conversation_id: &str,
        agent_id: &str,
    ) -> Result<ConversationAgentSession> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT native_session_id FROM conversation_agent_sessions
                 WHERE conversation_id = ?1 AND agent_id = ?2",
                rusqlite::params![conversation_id, agent_id],
                |row| {
                    Ok(ConversationAgentSession {
                        native_session_id: row.get(0)?,
                    })
                },
            )
            .map_err(Error::from)
        })
    }

    pub fn last_terminal_trigger_before(
        &self,
        conversation_id: &str,
        agent_id: &str,
        before_message_id: i64,
    ) -> Result<Option<i64>> {
        self.db.with_conn(|conn| {
            latest_terminal_trigger_before_message(
                conn,
                conversation_id,
                agent_id,
                before_message_id,
            )
        })
    }

    pub fn is_running(&self, turn_id: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM conversation_turns t
                    JOIN conversation_participants p
                      ON p.conversation_id = t.conversation_id
                     AND p.participant_type = 'agent'
                     AND p.participant_id = t.agent_id
                    WHERE t.id = ?1 AND t.status = 'running'
                 )",
                [turn_id],
                |row| row.get(0),
            )
            .map_err(Error::from)
        })
    }

    /// Cancel queued or running work, or dismiss a terminal failure. The
    /// durable transition happens before the caller interrupts a live ACP
    /// process, so a late response cannot be published after cancellation.
    pub fn cancel(
        &self,
        conversation_id: &str,
        turn_id: &str,
    ) -> Result<ConversationTurnCancellation> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let original = transaction
                .query_row(
                    "SELECT * FROM conversation_turns
                     WHERE id = ?1 AND conversation_id = ?2",
                    rusqlite::params![turn_id, conversation_id],
                    row_to_turn,
                )
                .optional()?
                .ok_or_else(|| Error::Conversation(format!("turn {turn_id} not found")))?;
            let was_running = original.status == "running";
            let changed = transaction.execute(
                "UPDATE conversation_turns
                 SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                     error_message = NULL
                 WHERE id = ?1 AND conversation_id = ?2
                   AND status IN ('queued', 'running', 'failed')",
                rusqlite::params![turn_id, conversation_id],
            )? == 1;
            if changed && was_running {
                // The native ACP session may already contain the cancelled
                // prompt. Deferred work must start from reconstructed visible
                // history instead of resuming that session.
                transaction.execute(
                    "UPDATE conversation_agent_sessions
                     SET native_session_id = NULL, updated_at = CURRENT_TIMESTAMP
                     WHERE conversation_id = ?1 AND agent_id = ?2",
                    rusqlite::params![&original.conversation_id, &original.agent_id],
                )?;
                let after_id = original.trigger_message_id.unwrap_or(0);
                if let Some((message_id, _)) = latest_addressed_message(
                    &transaction,
                    &original.conversation_id,
                    &original.agent_id,
                    after_id,
                    None,
                )? {
                    Self::enqueue_in_transaction(
                        &transaction,
                        &original.conversation_id,
                        &original.agent_id,
                        message_id,
                    )?;
                }
            }
            if changed {
                refresh_agent_session_state(
                    &transaction,
                    &original.conversation_id,
                    &original.agent_id,
                )?;
            }
            let turn = transaction.query_row(
                "SELECT * FROM conversation_turns WHERE id = ?1",
                [turn_id],
                row_to_turn,
            )?;
            transaction.commit()?;
            Ok(ConversationTurnCancellation {
                turn,
                changed,
                was_running,
            })
        })
    }

    /// Store an Agent response and its copied artifacts, then finish the turn
    /// in the same transaction. A participant removed while the prompt is
    /// running cannot publish a late response: cancellation wins before any
    /// of those records become visible.
    pub fn complete_with_message(
        &self,
        turn: &ConversationTurn,
        native_session_id: &str,
        message: &SendMessage,
        metadata: &serde_json::Value,
    ) -> Result<Option<ConversationMessage>> {
        self.complete_with_message_and_visualizations(
            turn,
            native_session_id,
            message,
            metadata,
            &[],
            &[],
        )
    }

    pub fn complete_with_message_and_visualizations(
        &self,
        turn: &ConversationTurn,
        native_session_id: &str,
        message: &SendMessage,
        metadata: &serde_json::Value,
        visualizations: &[PreparedVisualization],
        published_files: &[PublishedFileAttachment],
    ) -> Result<Option<ConversationMessage>> {
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let may_publish = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM conversation_turns t
                    JOIN conversation_participants p
                      ON p.conversation_id = t.conversation_id
                     AND p.participant_type = 'agent'
                     AND p.participant_id = t.agent_id
                    WHERE t.id = ?1 AND t.status = 'running'
                 )",
                [&turn.id],
                |row| row.get::<_, bool>(0),
            )?;
            if !may_publish {
                transaction.commit()?;
                return Ok(None);
            }

            transaction.execute(
                "INSERT INTO conversation_messages
                 (conversation_id, sender_type, sender_id, sender_name, content,
                  message_type, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    turn.conversation_id,
                    message.sender_type,
                    message.sender_id,
                    message.sender_name,
                    message.content,
                    message.message_type.as_deref().unwrap_or("message"),
                    metadata.to_string(),
                ],
            )?;
            let message_id = transaction.last_insert_rowid();
            transaction.execute(
                "UPDATE conversations
                 SET last_message_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                [&turn.conversation_id],
            )?;
            let stored = transaction.query_row(
                "SELECT * FROM conversation_messages WHERE id = ?1",
                [message_id],
                row_to_message,
            )?;
            store_conversation_message_visualizations(
                &transaction,
                message_id,
                None,
                Some(&turn.id),
                visualizations,
            )?;
            for attachment in published_files {
                transaction.execute(
                    "INSERT INTO conversation_message_attachments
                     (id, message_id, name, mime_type, data, size, source_task_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                    rusqlite::params![
                        Uuid::new_v4().to_string(),
                        message_id,
                        attachment.name,
                        attachment.mime_type,
                        attachment.data,
                        attachment.data.len() as i64,
                    ],
                )?;
            }
            Self::enqueue_for_message_in_transaction(
                &transaction,
                &turn.conversation_id,
                message_id,
                &message.sender_type,
                &message.sender_id,
                &message.content,
            )?;
            Self::finish_in_transaction(
                &transaction,
                turn,
                "completed",
                Some(native_session_id),
                Some(message_id),
                None,
            )?;
            transaction.commit()?;
            Ok(Some(stored))
        })
    }

    pub fn complete(
        &self,
        turn: &ConversationTurn,
        native_session_id: &str,
        result_message_id: i64,
    ) -> Result<()> {
        self.finish(
            turn,
            "completed",
            Some(native_session_id),
            Some(result_message_id),
            None,
        )
    }

    pub fn fail(&self, turn: &ConversationTurn, error: &str) -> Result<()> {
        self.finish(turn, "failed", None, None, Some(error))
    }

    fn finish(
        &self,
        turn: &ConversationTurn,
        status: &str,
        native_session_id: Option<&str>,
        result_message_id: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            Self::finish_in_transaction(
                &tx,
                turn,
                status,
                native_session_id,
                result_message_id,
                error,
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    fn finish_in_transaction(
        transaction: &rusqlite::Transaction<'_>,
        turn: &ConversationTurn,
        status: &str,
        native_session_id: Option<&str>,
        result_message_id: Option<i64>,
        error: Option<&str>,
    ) -> Result<bool> {
        let updated = transaction.execute(
            "UPDATE conversation_turns
                 SET status = ?1, result_message_id = ?2, error_message = ?3,
                     completed_at = CURRENT_TIMESTAMP
                 WHERE id = ?4 AND status = 'running'",
            rusqlite::params![status, result_message_id, error, turn.id],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        transaction.execute(
            "UPDATE conversation_agent_sessions
                 SET native_session_id = COALESCE(?1, native_session_id), status = ?2,
                     last_error = ?3, updated_at = CURRENT_TIMESTAMP
                 WHERE conversation_id = ?4 AND agent_id = ?5",
            rusqlite::params![
                native_session_id,
                if status == "completed" {
                    "idle"
                } else {
                    "failed"
                },
                error,
                turn.conversation_id,
                turn.agent_id,
            ],
        )?;

        let after_id = turn.trigger_message_id.unwrap_or(0);
        let latest_addressed = latest_addressed_message(
            transaction,
            &turn.conversation_id,
            &turn.agent_id,
            after_id,
            None,
        )?;
        if let Some((latest_addressed, response_queued_at)) = latest_addressed {
            let id = Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT INTO conversation_turns
                     (id, conversation_id, agent_id, trigger_message_id, response_queued_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    id,
                    turn.conversation_id,
                    turn.agent_id,
                    latest_addressed,
                    response_queued_at
                ],
            )?;
            transaction.execute(
                "UPDATE conversation_agent_sessions
                     SET status = 'queued', last_error = NULL, updated_at = CURRENT_TIMESTAMP
                     WHERE conversation_id = ?1 AND agent_id = ?2",
                rusqlite::params![turn.conversation_id, turn.agent_id],
            )?;
        }
        Ok(true)
    }

    pub fn set_context_usage(&self, turn_id: &str, used: u64, size: u64) -> Result<()> {
        let used = i64::try_from(used)
            .map_err(|_| Error::Backend("ACP context usage exceeds SQLite range".to_string()))?;
        let size = i64::try_from(size)
            .map_err(|_| Error::Backend("ACP context size exceeds SQLite range".to_string()))?;
        self.db.with_conn(|conn| {
            let updated = conn.execute(
                "UPDATE conversation_turns SET context_used = ?1, context_size = ?2 WHERE id = ?3",
                rusqlite::params![used, size, turn_id],
            )?;
            if updated == 0 {
                return Err(Error::Conversation(format!("turn {turn_id} not found")));
            }
            Ok(())
        })?;
        Ok(())
    }

    pub fn recover(&self) -> Result<usize> {
        self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let changed = transaction.execute(
                "UPDATE conversation_turns
                 SET status = 'queued', started_at = NULL,
                     response_queued_at = CURRENT_TIMESTAMP, response_started_at = NULL,
                     error_message = 'Recovered after XpressClaw restart'
                 WHERE status = 'running'",
                [],
            )?;
            transaction.execute(
                "UPDATE conversation_agent_sessions SET status = 'queued'
                 WHERE status = 'running'",
                [],
            )?;
            finalize_inactive_git_baselines(&transaction, None)?;
            transaction.commit()?;
            Ok(changed)
        })
    }

    pub fn list_for_conversation(
        &self,
        conversation_id: &str,
        limit: i64,
    ) -> Result<Vec<ConversationTurn>> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT * FROM conversation_turns WHERE conversation_id = ?1
                 ORDER BY queued_at DESC, rowid DESC LIMIT ?2",
            )?;
            let turns = statement
                .query_map(rusqlite::params![conversation_id, limit], row_to_turn)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(turns)
        })
    }
}

/// Reconcile responses when a message is deleted. Coalesced queued turns are
/// retargeted when earlier visible work remains; all other unfinished or
/// failed responses that may have consumed the message are cancelled. Server
/// callers use the returned running IDs to interrupt the corresponding ACP
/// processes.
pub(crate) fn reconcile_message_deletion_turns(
    connection: &rusqlite::Connection,
    conversation_id: &str,
    message_id: i64,
) -> Result<Vec<String>> {
    let mut queued_statement = connection.prepare(
        "SELECT id, agent_id FROM conversation_turns
         WHERE conversation_id = ?1 AND trigger_message_id = ?2 AND status = 'queued'",
    )?;
    let queued_turns = queued_statement
        .query_map(rusqlite::params![conversation_id, message_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(queued_statement);
    for (turn_id, agent_id) in queued_turns {
        retarget_queued_turn_before_message(
            connection,
            conversation_id,
            &turn_id,
            &agent_id,
            message_id,
        )?;
    }

    // A native session may retain any completed prompt in its own history.
    // Once a message is hidden, no Agent in this conversation may resume a
    // session that could still contain it.
    connection.execute(
        "UPDATE conversation_agent_sessions
         SET native_session_id = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE conversation_id = ?1",
        [conversation_id],
    )?;

    let mut running_statement = connection.prepare(
        "SELECT id, agent_id FROM conversation_turns
         WHERE conversation_id = ?1 AND trigger_message_id >= ?2 AND status = 'running'",
    )?;
    let running_turns = running_statement
        .query_map(rusqlite::params![conversation_id, message_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(running_statement);

    let mut agent_statement = connection.prepare(
        "SELECT DISTINCT agent_id FROM conversation_turns
         WHERE conversation_id = ?1
           AND ((trigger_message_id = ?2 AND status IN ('queued', 'failed'))
                OR (trigger_message_id >= ?2 AND status = 'running'))",
    )?;
    let affected_agents = agent_statement
        .query_map(rusqlite::params![conversation_id, message_id], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(agent_statement);

    connection.execute(
        "UPDATE conversation_turns
         SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
             error_message = NULL
         WHERE conversation_id = ?1
           AND ((trigger_message_id = ?2 AND status IN ('queued', 'failed'))
                OR (trigger_message_id >= ?2 AND status = 'running'))",
        rusqlite::params![conversation_id, message_id],
    )?;
    for (_, agent_id) in &running_turns {
        let replacement = match latest_addressed_message(
            connection,
            conversation_id,
            agent_id,
            message_id,
            None,
        )? {
            Some(message) => Some(message),
            None => {
                let last_terminal_trigger = latest_terminal_trigger_before_message(
                    connection,
                    conversation_id,
                    agent_id,
                    message_id,
                )?
                .unwrap_or(0);
                latest_addressed_message(
                    connection,
                    conversation_id,
                    agent_id,
                    last_terminal_trigger,
                    Some(message_id),
                )?
            }
        };
        if let Some((replacement_message_id, _)) = replacement {
            ConversationTurnQueue::enqueue_in_transaction(
                connection,
                conversation_id,
                agent_id,
                replacement_message_id,
            )?;
        }
    }
    connection.execute(
        "UPDATE conversation_turns SET result_message_id = NULL
         WHERE conversation_id = ?1 AND result_message_id = ?2",
        rusqlite::params![conversation_id, message_id],
    )?;
    for agent_id in affected_agents {
        refresh_agent_session_state(connection, conversation_id, &agent_id)?;
    }
    Ok(running_turns.into_iter().map(|(id, _)| id).collect())
}

/// A queued turn owns a high-water message, not only that message. If its
/// newest coalesced message is deleted, keep the response owed to any earlier
/// visible activity that still targets the same Agent.
fn retarget_queued_turn_before_message(
    connection: &rusqlite::Connection,
    conversation_id: &str,
    turn_id: &str,
    agent_id: &str,
    before_message_id: i64,
) -> Result<bool> {
    let last_terminal_trigger = latest_terminal_trigger_before_message(
        connection,
        conversation_id,
        agent_id,
        before_message_id,
    )?
    .unwrap_or(0);
    let candidate = latest_addressed_message(
        connection,
        conversation_id,
        agent_id,
        last_terminal_trigger,
        Some(before_message_id),
    )?;
    let Some((trigger_message_id, response_queued_at)) = candidate else {
        return Ok(false);
    };
    Ok(connection.execute(
        "UPDATE conversation_turns
         SET trigger_message_id = ?1, response_queued_at = ?2
         WHERE id = ?3 AND status = 'queued' AND trigger_message_id = ?4",
        rusqlite::params![
            trigger_message_id,
            response_queued_at,
            turn_id,
            before_message_id
        ],
    )? == 1)
}

fn latest_terminal_trigger_before_message(
    connection: &rusqlite::Connection,
    conversation_id: &str,
    agent_id: &str,
    before_message_id: i64,
) -> Result<Option<i64>> {
    connection
        .query_row(
            "SELECT MAX(trigger_message_id) FROM conversation_turns
         WHERE conversation_id = ?1 AND agent_id = ?2
           AND status IN ('completed', 'cancelled', 'failed')
           AND trigger_message_id < ?3",
            rusqlite::params![conversation_id, agent_id, before_message_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(Error::from)
}

fn latest_addressed_message(
    connection: &rusqlite::Connection,
    conversation_id: &str,
    agent_id: &str,
    after_message_id: i64,
    before_message_id: Option<i64>,
) -> Result<Option<(i64, String)>> {
    let mut statement = connection.prepare(
        "SELECT id, sender_type, sender_id, content, metadata, created_at
         FROM conversation_messages
         WHERE conversation_id = ?1 AND id > ?2
           AND (?3 IS NULL OR id < ?3) AND deleted_at IS NULL
         ORDER BY id DESC",
    )?;
    let messages = statement.query_map(
        rusqlite::params![conversation_id, after_message_id, before_message_id],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        },
    )?;
    for message in messages {
        let (id, sender_type, sender_id, content, metadata, created_at) = message?;
        let agent_mentions = ConversationTurnQueue::agent_mentions(&content);
        let routed_target_agent_id = serde_json::from_str::<serde_json::Value>(&metadata)
            .ok()
            .and_then(|value| {
                value
                    .get("target_agent_id")
                    .and_then(|target| target.as_str())
                    .map(str::to_owned)
            });
        if ConversationTurnQueue::message_targets_agent(
            &sender_type,
            &sender_id,
            agent_id,
            &agent_mentions,
            routed_target_agent_id.as_deref(),
        ) {
            return Ok(Some((id, created_at)));
        }
    }
    Ok(None)
}

fn refresh_agent_session_state(
    connection: &rusqlite::Connection,
    conversation_id: &str,
    agent_id: &str,
) -> Result<()> {
    let latest = connection
        .query_row(
            "SELECT status, error_message FROM conversation_turns
             WHERE conversation_id = ?1 AND agent_id = ?2
             ORDER BY queued_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![conversation_id, agent_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()?;
    let (status, last_error) = match latest {
        Some((status, error)) if matches!(status.as_str(), "queued" | "running" | "failed") => {
            (status, error)
        }
        _ => ("idle".to_string(), None),
    };
    connection.execute(
        "UPDATE conversation_agent_sessions
         SET status = ?1, last_error = ?2, updated_at = CURRENT_TIMESTAMP
         WHERE conversation_id = ?3 AND agent_id = ?4",
        rusqlite::params![status, last_error, conversation_id, agent_id],
    )?;
    Ok(())
}

fn row_to_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationTurn> {
    Ok(ConversationTurn {
        id: row.get("id")?,
        conversation_id: row.get("conversation_id")?,
        agent_id: row.get("agent_id")?,
        trigger_message_id: row.get("trigger_message_id")?,
        status: row.get("status")?,
        result_message_id: row.get("result_message_id")?,
        error_message: row.get("error_message")?,
        context_used: row.get("context_used")?,
        context_size: row.get("context_size")?,
        queued_at: row.get("queued_at")?,
        started_at: row.get("started_at")?,
        completed_at: row.get("completed_at")?,
        response_queued_at: row.get("response_queued_at")?,
        response_started_at: row.get("response_started_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::{CreateConversation, SendMessage};

    fn setup() -> (Arc<Database>, ConversationManager, ConversationTurnQueue) {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name) VALUES ('p', 'Project')",
                [],
            )?;
            for id in ["atlas", "reviewer"] {
                conn.execute(
                    "INSERT INTO agents (id, name, backend, config, project_id)
                     VALUES (?1, ?1, 'native', '{}', 'p')",
                    [id],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();
        let manager = ConversationManager::new(db.clone());
        let queue = ConversationTurnQueue::new(db.clone());
        (db, manager, queue)
    }

    #[test]
    fn conversation_turns_break_queued_at_ties_by_insertion_order() {
        let (db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Ordered failures".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        db.with_conn(|conn| {
            for (id, status) in [("older", "failed"), ("newer", "cancelled")] {
                conn.execute(
                    "INSERT INTO conversation_turns
                     (id, conversation_id, agent_id, status, queued_at)
                     VALUES (?1, ?2, 'atlas', ?3, '2026-01-01 00:00:00')",
                    rusqlite::params![id, conversation.id, status],
                )?;
            }
            Ok::<_, rusqlite::Error>(())
        })
        .unwrap();

        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();

        assert_eq!(
            turns
                .iter()
                .map(|turn| turn.id.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older"]
        );
    }

    #[test]
    fn user_messages_queue_every_participant_and_coalesce_while_queued() {
        let (_db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into(), "reviewer".into()],
                },
            )
            .unwrap();
        let first = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "Please investigate".into(),
                    message_type: None,
                },
            )
            .unwrap();
        let queued = queue
            .enqueue_for_message(&conversation.id, first.id, "user", "local", &first.content)
            .unwrap();
        assert_eq!(queued, vec!["atlas", "reviewer"]);

        let second = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "More context".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue_for_message(
                &conversation.id,
                second.id,
                "user",
                "local",
                &second.content,
            )
            .unwrap();
        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns.iter().all(|turn| {
            turn.trigger_message_id == Some(second.id)
                && turn.response_queued_at.as_deref() == Some(second.created_at.as_str())
                && turn.response_started_at.is_none()
        }));
    }

    #[test]
    fn failed_turn_can_be_dismissed_and_stops_needing_attention() {
        let (db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Failure".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        let message = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "Please answer".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue(&conversation.id, "atlas", message.id)
            .unwrap();
        let running = queue.claim_next().unwrap().unwrap();
        queue.fail(&running, "Harness refused the request").unwrap();

        let before = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT needs_attention FROM dashboard_events
                     WHERE work_kind = 'conversation_turn' AND work_id = ?1",
                    [&running.id],
                    |row| row.get::<_, bool>(0),
                )
            })
            .unwrap();
        assert!(before);

        let cancellation = queue.cancel(&conversation.id, &running.id).unwrap();

        assert!(cancellation.changed);
        assert!(!cancellation.was_running);
        assert_eq!(cancellation.turn.status, "cancelled");
        let (session_status, still_needs_attention) = db
            .with_conn(|conn| {
                Ok::<_, rusqlite::Error>((
                    conn.query_row(
                        "SELECT status FROM conversation_agent_sessions
                         WHERE conversation_id = ?1 AND agent_id = 'atlas'",
                        [&conversation.id],
                        |row| row.get::<_, String>(0),
                    )?,
                    conn.query_row(
                        "SELECT COUNT(*) FROM dashboard_events
                         WHERE work_kind = 'conversation_turn' AND work_id = ?1
                           AND needs_attention = 1",
                        [&running.id],
                        |row| row.get::<_, i64>(0),
                    )?,
                ))
            })
            .unwrap();
        assert_eq!(session_status, "idle");
        assert_eq!(still_needs_attention, 0);
    }

    #[test]
    fn recovery_closes_interrupted_conversation_git_baseline() {
        let (db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Recovery".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        let message = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "Please continue after restart".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue_for_message(
                &conversation.id,
                message.id,
                "user",
                "local",
                &message.content,
            )
            .unwrap();
        let running = queue.claim_next().unwrap().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO dashboard_git_baselines
                     (work_kind, work_id, workspace, baseline_json, git_state)
                 VALUES (
                     'conversation_turn', ?1, '/tmp/recovered-conversation', '{}', 'available'
                 )",
                [&running.id],
            )
        })
        .unwrap();

        assert_eq!(queue.recover().unwrap(), 1);
        let recovered = queue
            .list_for_conversation(&conversation.id, 10)
            .unwrap()
            .into_iter()
            .find(|turn| turn.id == running.id)
            .unwrap();
        assert_eq!(recovered.status, "queued");
        assert!(db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT finalized_at FROM dashboard_git_baselines
                     WHERE work_kind = 'conversation_turn' AND work_id = ?1",
                    [&running.id],
                    |row| row.get::<_, Option<String>>(0),
                )
            })
            .unwrap()
            .is_some());
    }

    #[test]
    fn routed_messages_roll_back_when_their_agent_turns_cannot_be_stored() {
        let (db, manager, _queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        db.with_conn(|conn| conn.execute("DROP TABLE conversation_turns", []))
            .unwrap();

        let result = manager.send_routed_message_with_attachments(
            &conversation.id,
            &SendMessage {
                sender_type: "user".into(),
                sender_id: "local".into(),
                sender_name: None,
                content: "This must not be stranded".into(),
                message_type: None,
            },
            None,
            None,
            &[],
        );

        assert!(result.is_err());
        let stored_messages: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_messages WHERE conversation_id = ?1",
                    [&conversation.id],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(stored_messages, 0);
    }

    #[test]
    fn completing_an_agent_response_atomically_routes_mentioned_peers() {
        let (_db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into(), "reviewer".into()],
                },
            )
            .unwrap();
        let request = manager
            .send_routed_message_with_attachments(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "@[AGENT:atlas:Atlas] investigate".into(),
                    message_type: None,
                },
                None,
                None,
                &[],
            )
            .unwrap()
            .0;
        let running = queue.claim_next().unwrap().unwrap();
        assert_eq!(running.trigger_message_id, Some(request.id));

        let response = queue
            .complete_with_message_and_visualizations(
                &running,
                "session",
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: Some("Atlas".into()),
                    content: "@[AGENT:reviewer:Reviewer] please verify this".into(),
                    message_type: None,
                },
                &serde_json::json!({}),
                &[],
                &[PublishedFileAttachment {
                    name: "Review.pptx".into(),
                    mime_type:
                        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                            .into(),
                    data: b"presentation".to_vec(),
                }],
            )
            .unwrap()
            .unwrap();
        let attachments = manager.attachments(response.id).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].name, "Review.pptx");

        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();
        assert!(turns.iter().any(|turn| {
            turn.agent_id == "reviewer"
                && turn.status == "queued"
                && turn.trigger_message_id == Some(response.id)
        }));
    }

    #[test]
    fn messages_arriving_during_a_turn_are_queued_after_it_completes() {
        let (_db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        let first = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "Start".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue_for_message(&conversation.id, first.id, "user", "local", &first.content)
            .unwrap();
        let running = queue.claim_next().unwrap().unwrap();
        assert_eq!(running.agent_id, "atlas");
        assert!(running.response_started_at.is_none());
        assert!(queue.start_response(&running.id).unwrap());
        let responding = queue
            .list_for_conversation(&conversation.id, 10)
            .unwrap()
            .into_iter()
            .find(|turn| turn.id == running.id)
            .unwrap();
        assert!(responding.response_started_at.is_some());
        assert!(!queue.start_response(&running.id).unwrap());

        let second = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "More context".into(),
                    message_type: None,
                },
            )
            .unwrap();
        assert!(queue
            .enqueue_for_message(
                &conversation.id,
                second.id,
                "user",
                "local",
                &second.content
            )
            .unwrap()
            .is_empty());
        let result = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: None,
                    content: "First result".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue.complete(&running, "session", result.id).unwrap();

        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();
        assert_eq!(
            turns
                .iter()
                .filter(|turn| turn.status == "completed")
                .count(),
            1
        );
        assert_eq!(
            turns.iter().filter(|turn| turn.status == "queued").count(),
            1
        );
        let follow_up = turns.iter().find(|turn| turn.status == "queued").unwrap();
        assert_eq!(follow_up.trigger_message_id, Some(second.id));
        assert_eq!(
            follow_up.response_queued_at.as_deref(),
            Some(second.created_at.as_str())
        );
    }

    #[test]
    fn targeted_messages_arriving_during_a_turn_are_queued_for_the_target() {
        let (_db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into(), "reviewer".into()],
                },
            )
            .unwrap();
        let first = manager
            .send_routed_message_with_attachments(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "@[AGENT:atlas:Atlas] Start".into(),
                    message_type: None,
                },
                None,
                None,
                &[],
            )
            .unwrap()
            .0;
        let running = queue.claim_next().unwrap().unwrap();
        assert_eq!(running.agent_id, "atlas");
        assert_eq!(running.trigger_message_id, Some(first.id));

        let (follow_up, _, queued_agents) = manager
            .send_targeted_routed_message_with_attachments(
                &conversation.id,
                "atlas",
                &SendMessage {
                    sender_type: "system".into(),
                    sender_id: "scheduler".into(),
                    sender_name: Some("XpressClaw".into()),
                    content: "Scheduled wake-up".into(),
                    message_type: Some("scheduled_wakeup".into()),
                },
                None,
                &[],
            )
            .unwrap();
        assert!(queued_agents.is_empty());

        let result = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: None,
                    content: "First result".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue.complete(&running, "session", result.id).unwrap();

        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();
        assert!(turns.iter().any(|turn| {
            turn.agent_id == "atlas"
                && turn.status == "queued"
                && turn.trigger_message_id == Some(follow_up.id)
        }));
        assert!(!turns.iter().any(|turn| {
            turn.agent_id == "reviewer" && turn.trigger_message_id == Some(follow_up.id)
        }));
    }

    #[test]
    fn malformed_literal_mentions_follow_the_same_broadcast_routing_after_a_turn() {
        let (_db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        let first = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "Start".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue_for_message(&conversation.id, first.id, "user", "local", &first.content)
            .unwrap();
        let running = queue.claim_next().unwrap().unwrap();

        let follow_up = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "The literal prefix `@[AGENT:` is not a mention".into(),
                    message_type: None,
                },
            )
            .unwrap();
        assert!(ConversationManager::parse_mentions(&follow_up.content).is_empty());
        assert!(queue
            .enqueue_for_message(
                &conversation.id,
                follow_up.id,
                "user",
                "local",
                &follow_up.content,
            )
            .unwrap()
            .is_empty());
        let result = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: None,
                    content: "First result".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue.complete(&running, "session", result.id).unwrap();

        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();
        assert!(turns.iter().any(|turn| {
            turn.status == "queued" && turn.trigger_message_id == Some(follow_up.id)
        }));
    }

    #[test]
    fn removing_a_participant_cancels_its_turn_and_rejects_a_late_response() {
        let (_db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into()],
                },
            )
            .unwrap();
        let request = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "Please investigate".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue_for_message(
                &conversation.id,
                request.id,
                "user",
                "local",
                &request.content,
            )
            .unwrap();
        let running = queue.claim_next().unwrap().unwrap();

        assert_eq!(
            manager
                .remove_participant(&conversation.id, "agent", "atlas")
                .unwrap(),
            vec![running.id.clone()]
        );
        assert!(!queue.is_running(&running.id).unwrap());
        assert!(!queue
            .enqueue(&conversation.id, "atlas", request.id)
            .unwrap());
        assert!(queue
            .complete_with_message(
                &running,
                "native-session",
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: Some("Atlas".into()),
                    content: "This response arrived too late".into(),
                    message_type: None,
                },
                &serde_json::json!({}),
            )
            .unwrap()
            .is_none());

        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();
        assert_eq!(turns[0].status, "cancelled");
        let messages = manager.get_messages(&conversation.id, 10, None).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender_type, "user");
    }

    #[test]
    fn a_user_mention_does_not_requeue_unaddressed_running_agents() {
        let (_db, manager, queue) = setup();
        let conversation = manager
            .create_in_project(
                Some("p"),
                &CreateConversation {
                    title: Some("Plan".into()),
                    icon: None,
                    participant_ids: vec!["atlas".into(), "reviewer".into()],
                },
            )
            .unwrap();
        let first = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "@[AGENT:atlas:Atlas] start".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue_for_message(&conversation.id, first.id, "user", "local", &first.content)
            .unwrap();
        let running = queue.claim_next().unwrap().unwrap();

        let reviewer_message = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "user".into(),
                    sender_id: "local".into(),
                    sender_name: None,
                    content: "@[AGENT:reviewer:Reviewer] take a look".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue
            .enqueue_for_message(
                &conversation.id,
                reviewer_message.id,
                "user",
                "local",
                &reviewer_message.content,
            )
            .unwrap();
        let result = manager
            .send_message(
                &conversation.id,
                &SendMessage {
                    sender_type: "agent".into(),
                    sender_id: "atlas".into(),
                    sender_name: None,
                    content: "Done".into(),
                    message_type: None,
                },
            )
            .unwrap();
        queue.complete(&running, "session", result.id).unwrap();

        let turns = queue.list_for_conversation(&conversation.id, 10).unwrap();
        assert!(!turns
            .iter()
            .any(|turn| turn.agent_id == "atlas" && turn.status == "queued"));
        assert!(turns
            .iter()
            .any(|turn| turn.agent_id == "reviewer" && turn.status == "queued"));
    }
}
