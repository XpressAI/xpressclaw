use std::sync::Arc;

use chrono::DateTime;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::conversations::runtime::ConversationTurnQueue;
use crate::db::Database;
use crate::error::{Error, Result};
use crate::projects::ensure_project_accepts_work;

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: String,
    pub local_project_id: String,
    pub local_agent_id: String,
    pub project_id: String,
    pub agent_name: String,
    pub generation: i64,
    pub active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnPayload {
    pub text: String,
    pub history: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    pub version: u32,
    pub id: String,
    pub binding: Binding,
    pub kind: String,
    pub work_id: String,
    pub source_conversation_id: Option<String>,
    pub payload: TurnPayload,
    pub expires_at: String,
    pub cancel_requested: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub status: String,
    pub text: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Execution {
    pub id: String,
    pub conversation_id: Option<String>,
    pub turn_id: Option<String>,
    pub receipt: Receipt,
    pub acknowledged: bool,
    pub lease_until: i64,
}

/// A connected runner gets a purpose-bound callback capability, never the
/// control plane's root credential or a local task-administration capability.
pub fn callback_capability(secret: &str, agent: &str) -> String {
    crate::repositories::agent_callback_capability(secret, &format!("connect:{agent}"))
}

pub fn verify_callback_capability(secret: &str, agent: &str, supplied: &str) -> bool {
    crate::repositories::verify_agent_callback_capability(
        secret,
        &format!("connect:{agent}"),
        supplied,
    )
}

pub struct ConnectJournal {
    db: Arc<Database>,
}

impl ConnectJournal {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn bind(&self, instance: &str, binding: &Binding) -> Result<()> {
        if Uuid::parse_str(&binding.id).is_err() || binding.generation < 1 {
            return Err(invalid("Invalid platform binding"));
        }
        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            ensure_project_accepts_work(&tx, &binding.local_project_id)?;
            let project: Option<String> = tx.query_row(
                "SELECT project_id FROM agents WHERE id = ?1", [&binding.local_agent_id], |row| row.get(0),
            ).optional()?.flatten();
            if project.as_deref() != Some(binding.local_project_id.as_str()) {
                return Err(invalid("Agent does not belong to the selected local Project"));
            }
            let existing: Option<(String, String)> = tx.query_row(
                "SELECT instance_id, binding_json FROM connect_bindings WHERE id = ?1", [&binding.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let json = serde_json::to_string(binding)?;
            if let Some((owner, saved)) = existing {
                let previous: Binding = serde_json::from_str(&saved)?;
                let mut expected = previous.clone();
                expected.generation = binding.generation;
                expected.active = binding.active;
                if owner != instance || expected != *binding || binding.generation < previous.generation {
                    return Err(invalid("Binding identity changed"));
                }
                tx.execute("UPDATE connect_bindings SET binding_json = ?2, active = ?3, generation = ?4 WHERE id = ?1", params![binding.id, json, binding.active, binding.generation])?;
                if binding.generation > previous.generation {
                    tx.execute("UPDATE connect_commands SET lease_until = 0 WHERE binding_id = ?1 AND status = 'accepted'", [&binding.id])?;
                }
            } else {
                tx.execute("INSERT INTO connect_bindings (id, instance_id, local_project_id, local_agent_id, generation, active, binding_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![binding.id, instance, binding.local_project_id, binding.local_agent_id, binding.generation, binding.active, json])?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn bindings(&self, instance: &str) -> Result<Vec<Binding>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT binding_json, active FROM connect_bindings WHERE instance_id = ?1 ORDER BY local_agent_id")?;
            let rows = stmt.query_map([instance], |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)))?;
            rows.map(|row| {
                let (json, active) = row?;
                let mut binding: Binding = serde_json::from_str(&json)?;
                binding.active = active;
                Ok(binding)
            }).collect()
        })
    }

    pub fn disable_binding(&self, instance: &str, binding: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("UPDATE connect_bindings SET active = 0 WHERE instance_id = ?1 AND id = ?2", params![instance, binding])?;
            tx.execute("UPDATE connect_commands SET lease_until = 0 WHERE instance_id = ?1 AND binding_id = ?2 AND status = 'accepted'", params![instance, binding])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn admit(
        &self,
        instance: &str,
        command: &Command,
        lease_until: i64,
        now: i64,
    ) -> Result<Execution> {
        if command.version != 1
            || Uuid::parse_str(&command.id).is_err()
            || !matches!(command.kind.as_str(), "chat_turn" | "task_turn")
            || command.payload.text.len() > 524288
            || command.payload.history.len() > 100
        {
            return Err(invalid("Unsupported or oversized platform command"));
        }
        let expires = DateTime::parse_from_rfc3339(&command.expires_at)
            .map_err(|_| invalid("Invalid command expiry"))?
            .timestamp();
        let mut immutable = command.clone();
        immutable.cancel_requested = false;
        immutable.binding.active = true;
        let encoded = serde_json::to_vec(&immutable)?;
        if encoded.len() > 1024 * 1024 {
            return Err(invalid("Platform context is too large"));
        }
        let digest = format!("{:x}", Sha256::digest(encoded));
        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let previous: Option<(String, String)> = tx.query_row(
                "SELECT instance_id, payload_hash FROM connect_commands WHERE id = ?1", [&command.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((owner, hash)) = previous {
                if owner != instance || hash != digest { return Err(invalid("Command ID conflicts with an existing receipt")); }
                tx.commit()?;
                return execution(conn, &command.id);
            }
            let saved: Option<String> = tx.query_row(
                "SELECT binding_json FROM connect_bindings WHERE id = ?1 AND instance_id = ?2",
                params![command.binding.id, instance], |row| row.get(0),
            ).optional()?;
            let mut identity = command.binding.clone();
            identity.active = true;
            if saved.as_deref() != Some(serde_json::to_string(&identity)?.as_str()) {
                return Err(invalid("Command does not match an approved local binding"));
            }
            let cancelled = command.cancel_requested || expires <= now;
            if !cancelled {
            let active: bool = tx.query_row("SELECT active FROM connect_bindings WHERE id = ?1", [&command.binding.id], |row| row.get(0))?;
            if !active || !command.binding.active { return Err(invalid("Binding is disabled")); }
            ensure_project_accepts_work(&tx, &command.binding.local_project_id)?;
            let project: Option<String> = tx.query_row("SELECT project_id FROM agents WHERE id = ?1", [&command.binding.local_agent_id], |row| row.get(0)).optional()?.flatten();
            if project.as_deref() != Some(command.binding.local_project_id.as_str()) { return Err(invalid("Bound Agent has moved or was deleted")); }
            }
            if !cancelled && lease_until <= now { return Err(invalid("Execution authorization has expired")); }
            let (conversation, turn) = if cancelled { (None, None) } else {
                let conversation = Uuid::new_v4().to_string();
                tx.execute("INSERT INTO conversations (id, title, project_id) VALUES (?1, ?2, ?3)",
                    params![conversation, format!("Xpress AI · {} {}", command.kind, command.work_id), command.binding.local_project_id])?;
                tx.execute("INSERT INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, 'agent', ?2)", params![conversation, command.binding.local_agent_id])?;
                let context = command.payload.history.join("\n");
                let content = format!("This turn belongs to a connected Xpress AI {}.\n\nRecent platform history:\n{}\n\nCurrent request:\n{}", command.kind, context, command.payload.text);
                tx.execute("INSERT INTO conversation_messages (conversation_id, sender_type, sender_id, sender_name, content) VALUES (?1, 'user', 'xpress-ai', 'Xpress AI', ?2)", params![conversation, content])?;
                let trigger = tx.last_insert_rowid();
                ConversationTurnQueue::enqueue_target_in_transaction(&tx, &conversation, &command.binding.local_agent_id, trigger)?;
                let turn: String = tx.query_row("SELECT id FROM conversation_turns WHERE conversation_id = ?1", [&conversation], |row| row.get(0))?;
                (Some(conversation), Some(turn))
            };
            tx.execute("INSERT INTO connect_commands (id, instance_id, binding_id, payload_hash, conversation_id, turn_id, status, lease_until, binding_generation) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![command.id, instance, command.binding.id, digest, conversation, turn, if cancelled { "cancelled" } else { "accepted" }, lease_until, command.binding.generation])?;
            tx.commit()?;
            execution(conn, &command.id)
        })
    }

    pub fn is_linked(&self, conversation: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM connect_commands WHERE conversation_id = ?1)",
                [conversation],
                |row| row.get(0),
            )?)
        })
    }

    pub fn tool_command(
        &self,
        instance: &str,
        conversation: &str,
        agent: &str,
        now: i64,
    ) -> Result<String> {
        self.db.with_conn(|conn| {
            conn.query_row("SELECT c.id FROM connect_commands c JOIN connect_bindings b ON b.id = c.binding_id JOIN agents a ON a.id = b.local_agent_id AND a.project_id = b.local_project_id JOIN projects p ON p.id = b.local_project_id AND p.deletion_started_at IS NULL WHERE c.instance_id = ?1 AND c.conversation_id = ?2 AND b.local_agent_id = ?3 AND b.active = 1 AND b.generation = c.binding_generation AND c.status = 'accepted' AND c.lease_until > ?4",
                params![instance, conversation, agent, now], |row| row.get(0)).optional()?.ok_or_else(|| invalid("No authorized connected turn for this Agent"))
        })
    }

    pub fn pending(&self, instance: &str) -> Result<Vec<Execution>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id FROM connect_commands WHERE instance_id = ?1 AND acknowledged = 0 ORDER BY created_at, id")?;
            let ids = stmt.query_map([instance], |row| row.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
            ids.iter().map(|id| execution(conn, id)).collect()
        })
    }

    pub fn renew(&self, instance: &str, id: &str, lease_until: i64) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute("UPDATE connect_commands SET lease_until = ?3 WHERE id = ?1 AND instance_id = ?2 AND status = 'accepted' AND binding_id IN (SELECT b.id FROM connect_bindings b JOIN agents a ON a.id = b.local_agent_id AND a.project_id = b.local_project_id JOIN projects p ON p.id = b.local_project_id AND p.deletion_started_at IS NULL WHERE b.active = 1 AND b.generation = connect_commands.binding_generation)", params![id, instance, lease_until])?;
            Ok(())
        })
    }

    pub fn acknowledge(&self, instance: &str, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute("UPDATE connect_commands SET acknowledged = 1 WHERE id = ?1 AND instance_id = ?2 AND status <> 'accepted'", params![id, instance])?;
            Ok(())
        })
    }

    pub fn collect(&self, instance: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute("UPDATE connect_commands SET status = 'failed', result_error = 'execution_state_lost_after_restart' WHERE instance_id = ?1 AND status = 'accepted' AND turn_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM conversation_turns t WHERE t.id = connect_commands.turn_id)", [instance])?;
            conn.execute("UPDATE connect_commands SET
                status = (SELECT CASE t.status WHEN 'completed' THEN 'completed' WHEN 'cancelled' THEN 'cancelled' ELSE 'failed' END FROM conversation_turns t WHERE t.id = connect_commands.turn_id),
                result_text = (SELECT m.content FROM conversation_turns t JOIN conversation_messages m ON m.id = t.result_message_id WHERE t.id = connect_commands.turn_id),
                result_error = (SELECT t.error_message FROM conversation_turns t WHERE t.id = connect_commands.turn_id)
                WHERE instance_id = ?1 AND status = 'accepted' AND turn_id IN (SELECT id FROM conversation_turns WHERE status IN ('completed', 'failed', 'cancelled'))", [instance])?;
            Ok(())
        })
    }

    pub fn recover(&self) -> Result<()> {
        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute("UPDATE conversation_turns SET status = 'failed', error_message = 'execution_state_lost_after_restart', completed_at = CURRENT_TIMESTAMP WHERE id IN (SELECT turn_id FROM connect_commands WHERE status = 'accepted') AND status IN ('queued', 'running')", [])?;
            tx.commit()?;
            Ok(())
        })
    }
}

fn execution(conn: &rusqlite::Connection, id: &str) -> Result<Execution> {
    Ok(conn.query_row("SELECT id, conversation_id, turn_id, status, result_text, result_error, acknowledged, lease_until FROM connect_commands WHERE id = ?1", [id], |row| Ok(Execution {
        id: row.get(0)?, conversation_id: row.get(1)?, turn_id: row.get(2)?,
        receipt: Receipt { status: row.get(3)?, text: row.get(4)?, error: row.get(5)? },
        acknowledged: row.get(6)?, lease_until: row.get(7)?,
    }))?)
}

fn invalid(message: &str) -> Error {
    Error::ConfigValidation(message.into())
}
