pub mod event_bus;
pub mod processor;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
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
    pub created_at: String,
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
        created_at: row.get("created_at")?,
    })
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
        let id = Uuid::new_v4().to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conversations (id, title, icon) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, req.title, req.icon],
            )?;

            // Add the local user as participant
            conn.execute(
                "INSERT INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, 'user', 'local')",
                rusqlite::params![id],
            )?;

            // Add requested agent participants
            for agent_id in &req.participant_ids {
                conn.execute(
                    "INSERT OR IGNORE INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, 'agent', ?2)",
                    rusqlite::params![id, agent_id],
                )?;
            }

            Ok::<(), Error>(())
        })?;

        self.get(&id)
    }

    pub fn list(&self, limit: i64) -> Result<Vec<Conversation>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title, icon, created_at, updated_at, last_message_at
                 FROM conversations
                 ORDER BY COALESCE(last_message_at, created_at) DESC
                 LIMIT ?1",
            )?;

            let convs: Vec<Conversation> = stmt
                .query_map([limit], |row| {
                    Ok(Conversation {
                        id: row.get("id")?,
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
                "SELECT id, title, icon, created_at, updated_at, last_message_at
                 FROM conversations WHERE id = ?1",
            )?;

            let mut conv = stmt
                .query_row([id], |row| {
                    Ok(Conversation {
                        id: row.get("id")?,
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
        let deleted = self
            .db
            .with_conn(|conn| conn.execute("DELETE FROM conversations WHERE id = ?1", [id]))?;

        if deleted == 0 {
            return Err(Error::ConversationNotFound { id: id.to_string() });
        }
        Ok(())
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
        // Verify conversation exists
        let _ = self.get(conv_id)?;

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, ?2, ?3)",
                rusqlite::params![conv_id, participant_type, participant_id],
            )?;
            Ok::<(), Error>(())
        })
    }

    pub fn remove_participant(
        &self,
        conv_id: &str,
        participant_type: &str,
        participant_id: &str,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM conversation_participants WHERE conversation_id = ?1 AND participant_type = ?2 AND participant_id = ?3",
                rusqlite::params![conv_id, participant_type, participant_id],
            )?;
            Ok::<(), Error>(())
        })
    }

    pub fn send_message(&self, conv_id: &str, msg: &SendMessage) -> Result<ConversationMessage> {
        self.db.with_conn(|conn| {
            let message_type = msg.message_type.as_deref().unwrap_or("message");

            conn.execute(
                "INSERT INTO conversation_messages (conversation_id, sender_type, sender_id, sender_name, content, message_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![conv_id, msg.sender_type, msg.sender_id, msg.sender_name, msg.content, message_type],
            )?;

            let id = conn.last_insert_rowid();

            // Update conversation timestamps
            conn.execute(
                "UPDATE conversations SET last_message_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                [conv_id],
            )?;

            let mut stmt = conn.prepare("SELECT * FROM conversation_messages WHERE id = ?1")?;
            stmt.query_row([id], row_to_message)
                .map_err(|e| Error::Database(e.to_string()))
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
                     WHERE conversation_id = ?1 AND id < ?2
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
                        WHERE conversation_id = ?1
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
        let mentions = Self::parse_mentions(content);
        let mentioned_agents: Vec<String> = mentions
            .iter()
            .filter(|(t, _, _)| t == "AGENT")
            .map(|(_, id, _)| id.clone())
            .collect();

        if !mentioned_agents.is_empty() {
            return Ok(mentioned_agents);
        }

        // No explicit mention — auto-route to all agent participants
        let participants = self
            .db
            .with_conn(|conn| self.load_participants(conn, conv_id))?;
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

    /// Store a user message as unprocessed (processed=0).
    /// The background task will pick it up.
    pub fn send_user_message(
        &self,
        conv_id: &str,
        msg: &SendMessage,
    ) -> Result<ConversationMessage> {
        self.db.with_conn(|conn| {
            let message_type = msg.message_type.as_deref().unwrap_or("message");

            conn.execute(
                "INSERT INTO conversation_messages (conversation_id, sender_type, sender_id, sender_name, content, message_type, processed) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)",
                rusqlite::params![conv_id, msg.sender_type, msg.sender_id, msg.sender_name, msg.content, message_type],
            )?;

            let id = conn.last_insert_rowid();

            conn.execute(
                "UPDATE conversations SET last_message_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                [conv_id],
            )?;

            let mut stmt = conn.prepare("SELECT * FROM conversation_messages WHERE id = ?1")?;
            stmt.query_row([id], row_to_message)
                .map_err(|e| Error::Database(e.to_string()))
        })
    }

    /// Check if there are unprocessed user messages in a conversation.
    pub fn has_unprocessed(&self, conv_id: &str) -> bool {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM conversation_messages
                     WHERE conversation_id = ?1 AND sender_type = 'user' AND processed = 0",
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
                 WHERE conversation_id = ?1 AND id > ?2
                 ORDER BY id ASC",
            )?;
            let msgs = stmt
                .query_map(rusqlite::params![conv_id, after_id], row_to_message)?
                .filter_map(|r| r.ok())
                .collect();
            Ok(msgs)
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
