//! Routes connector events to the right destination.
//!
//! When a channel has `agent_id` set (direct binding), the event is injected
//! into a conversation with that agent. Otherwise, it's stored in the
//! `connector_events` table for the workflow engine to pick up.

use std::sync::Arc;

use serde_json::Value;
use tracing::{debug, error, info};

use crate::connectors::manager::ConnectorManager;
use crate::connectors::traits::ConnectorEvent;
use crate::conversations::{ConversationManager, CreateConversation, SendMessage};
use crate::db::Database;

/// Route a connector event to conversations (direct binding) or to the
/// connector_events table (for workflow matching).
///
/// Returns `Some((conv_id, agent_id))` if the message was injected into
/// a conversation (direct agent binding), so the caller can spawn the
/// conversation processor.
pub fn route_event(db: &Arc<Database>, event: &ConnectorEvent) -> Option<(String, String)> {
    let mgr = ConnectorManager::new(db.clone());

    // Look up the channel to check for direct agent binding
    let channel = match mgr.get_channel(&event.channel_id) {
        Ok(ch) => ch,
        Err(e) => {
            error!(
                channel_id = event.channel_id.as_str(),
                error = %e,
                "failed to look up channel for event routing"
            );
            return None;
        }
    };

    if let Some(ref agent_id) = channel.agent_id {
        // Direct binding: inject into a conversation with this agent
        return inject_into_conversation(db, event, agent_id);
    } else {
        // No agent binding: store for workflow engine
        if let Err(e) = mgr.record_event(
            &event.connector_id,
            &event.channel_id,
            &event.event_type,
            &event.payload,
        ) {
            error!(error = %e, "failed to record connector event");
        } else {
            debug!(
                connector_id = event.connector_id.as_str(),
                channel_id = event.channel_id.as_str(),
                event_type = event.event_type.as_str(),
                "connector event stored for workflow matching"
            );
        }
    }
    None
}

/// Find or create a conversation for this channel+agent and inject the message.
/// Returns (conv_id, agent_id) on success so the caller can spawn the processor.
fn inject_into_conversation(
    db: &Arc<Database>,
    event: &ConnectorEvent,
    agent_id: &str,
) -> Option<(String, String)> {
    let conv_mgr = ConversationManager::new(db.clone());

    // Extract text content from the event payload
    let text = extract_text(&event.payload);
    if text.is_empty() {
        debug!(
            channel_id = event.channel_id.as_str(),
            "skipping empty message from connector"
        );
        return None;
    }

    // Extract sender name from payload
    let sender_name = extract_sender_name(&event.payload)
        .unwrap_or_else(|| format!("{}:{}", event.connector_id, event.channel_id));

    // Find existing conversation for this channel, or create one
    let conv_id = match find_channel_conversation(db, &event.channel_id, agent_id) {
        Some(id) => id,
        None => {
            // Create a new conversation
            let connector_mgr = ConnectorManager::new(db.clone());
            let channel_name = connector_mgr
                .get_channel(&event.channel_id)
                .map(|ch| ch.name.clone())
                .unwrap_or_else(|_| event.channel_id.clone());

            match conv_mgr.create(&CreateConversation {
                title: Some(format!("#{channel_name}")),
                icon: Some("💬".to_string()),
                participant_ids: vec![agent_id.to_string()],
            }) {
                Ok(conv) => {
                    // Tag conversation with channel metadata so we can find it later
                    tag_conversation_channel(db, &conv.id, &event.channel_id, agent_id);
                    info!(
                        conv_id = conv.id.as_str(),
                        agent_id,
                        channel_id = event.channel_id.as_str(),
                        "created conversation for channel binding"
                    );
                    conv.id
                }
                Err(e) => {
                    error!(error = %e, "failed to create conversation for channel");
                    return None;
                }
            }
        }
    };

    // Send the message into the conversation (must use send_user_message
    // which sets processed=0 so the processor picks it up)
    match conv_mgr.send_user_message(
        &conv_id,
        &SendMessage {
            sender_type: "user".to_string(),
            sender_id: format!("connector:{}", event.connector_id),
            sender_name: Some(sender_name),
            content: text,
            message_type: None,
        },
    ) {
        Ok(_) => {
            info!(
                conv_id = conv_id.as_str(),
                agent_id,
                channel_id = event.channel_id.as_str(),
                "injected connector message into conversation"
            );
        }
        Err(e) => {
            error!(
                conv_id = conv_id.as_str(),
                error = %e,
                "failed to inject connector message"
            );
            return None;
        }
    }

    Some((conv_id, agent_id.to_string()))
}

/// Extract text content from various connector payload formats.
fn extract_text(payload: &Value) -> String {
    // Telegram: payload.text
    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        return text.to_string();
    }
    // Generic: payload.message or payload.content or payload.body
    for key in &["message", "content", "body", "description", "summary"] {
        if let Some(text) = payload.get(key).and_then(|v| v.as_str()) {
            return text.to_string();
        }
    }
    // File watcher: format as description
    if let Some(path) = payload.get("path").and_then(|v| v.as_str()) {
        let event = payload
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("changed");
        return format!("File {event}: {path}");
    }
    // Fallback: serialize the whole payload
    if !payload.is_null() {
        return serde_json::to_string_pretty(payload).unwrap_or_default();
    }
    String::new()
}

/// Extract a human-readable sender name from the event payload.
fn extract_sender_name(payload: &Value) -> Option<String> {
    // Telegram: from.first_name or from.username
    if let Some(from) = payload.get("from") {
        if let Some(name) = from.get("first_name").and_then(|v| v.as_str()) {
            return Some(name.to_string());
        }
        if let Some(username) = from.get("username").and_then(|v| v.as_str()) {
            return Some(format!("@{username}"));
        }
    }
    // Generic: payload.sender or payload.user or payload.author
    for key in &["sender", "user", "author"] {
        if let Some(name) = payload.get(key).and_then(|v| v.as_str()) {
            return Some(name.to_string());
        }
    }
    None
}

/// Find an existing conversation tagged with this channel+agent.
fn find_channel_conversation(
    db: &Arc<Database>,
    channel_id: &str,
    agent_id: &str,
) -> Option<String> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT conversation_id FROM conversation_channel_bindings
             WHERE channel_id = ?1 AND agent_id = ?2
             LIMIT 1",
            rusqlite::params![channel_id, agent_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    })
}

/// Tag a conversation with its channel binding for future lookup.
fn tag_conversation_channel(db: &Arc<Database>, conv_id: &str, channel_id: &str, agent_id: &str) {
    let _ = db.with_conn(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO conversation_channel_bindings
             (conversation_id, channel_id, agent_id)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![conv_id, channel_id, agent_id],
        )
        .map_err(|e| crate::error::Error::Database(e.to_string()))
    });
}
