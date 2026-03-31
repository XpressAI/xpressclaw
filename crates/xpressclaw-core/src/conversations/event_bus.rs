//! Per-conversation broadcast channel for SSE events (ADR-019).
//!
//! Connected clients subscribe to events. If no client is connected,
//! events are dropped — the stored message in the DB is the source of truth.

use std::collections::HashMap;
use std::sync::RwLock;

use serde::Serialize;
use tokio::sync::broadcast;

/// Events broadcast to connected clients for a conversation.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationEvent {
    /// Agent is about to start generating a response.
    Thinking { agent_id: String },
    /// A token chunk from the agent's streaming response.
    Chunk { agent_id: String, content: String },
    /// A complete message was stored (user or agent).
    Message {
        #[serde(flatten)]
        message: serde_json::Value,
    },
    /// An error occurred during processing.
    Error {
        agent_id: Option<String>,
        error: String,
    },
    /// Processing is complete for this cycle.
    Done,
}

const CHANNEL_CAPACITY: usize = 256;

/// Manages per-conversation broadcast channels.
pub struct ConversationEventBus {
    channels: RwLock<HashMap<String, broadcast::Sender<ConversationEvent>>>,
}

impl ConversationEventBus {
    pub fn new() -> Self {
        Self {
            channels: RwLock::new(HashMap::new()),
        }
    }

    /// Send an event to all subscribers of a conversation.
    /// If no subscribers, the event is silently dropped.
    pub fn send(&self, conv_id: &str, event: ConversationEvent) {
        let channels = self.channels.read().unwrap();
        if let Some(tx) = channels.get(conv_id) {
            // Ignore send errors — means no receivers are listening
            let _ = tx.send(event);
        }
    }

    /// Subscribe to events for a conversation.
    /// Creates the channel if it doesn't exist.
    pub fn subscribe(&self, conv_id: &str) -> broadcast::Receiver<ConversationEvent> {
        let mut channels = self.channels.write().unwrap();
        let tx = channels
            .entry(conv_id.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        tx.subscribe()
    }

    /// Clean up channels with no senders (conversation ended).
    pub fn cleanup(&self) {
        let mut channels = self.channels.write().unwrap();
        channels.retain(|_, tx| tx.receiver_count() > 0);
    }
}

impl Default for ConversationEventBus {
    fn default() -> Self {
        Self::new()
    }
}
