//! Broadcast bus for pi-agent container terminal output (stdout + stderr).
//!
//! The frontend "Logs" tab subscribes per agent_id via SSE to show a live
//! tmux-style view of what the pi container is printing.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::sync::RwLock;

/// One line of terminal output from a pi subprocess.
#[derive(Clone, Debug, serde::Serialize)]
pub struct PiTerminalLine {
    pub agent_id: String,
    /// "stdout" or "stderr".
    pub stream: &'static str,
    pub line: String,
    pub ts_ms: u64,
}

/// Per-agent broadcast channel. Keeps a recent tail in memory so newly
/// connected subscribers can replay the last N lines.
#[derive(Default)]
pub struct PiTerminalBus {
    channels: RwLock<HashMap<String, Arc<AgentChannel>>>,
}

struct AgentChannel {
    tx: broadcast::Sender<PiTerminalLine>,
    tail: RwLock<Vec<PiTerminalLine>>,
}

const TAIL_CAP: usize = 500;

impl PiTerminalBus {
    pub fn new() -> Self {
        Self::default()
    }

    async fn channel(&self, agent_id: &str) -> Arc<AgentChannel> {
        {
            let guard = self.channels.read().await;
            if let Some(ch) = guard.get(agent_id) {
                return ch.clone();
            }
        }
        let mut guard = self.channels.write().await;
        guard
            .entry(agent_id.to_string())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(256);
                Arc::new(AgentChannel {
                    tx,
                    tail: RwLock::new(Vec::with_capacity(TAIL_CAP)),
                })
            })
            .clone()
    }

    /// Publish a line. Fire-and-forget — no backpressure.
    pub async fn publish(&self, agent_id: &str, stream: &'static str, line: String) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        let entry = PiTerminalLine {
            agent_id: agent_id.to_string(),
            stream,
            line,
            ts_ms: now,
        };
        let ch = self.channel(agent_id).await;
        {
            let mut tail = ch.tail.write().await;
            if tail.len() >= TAIL_CAP {
                tail.remove(0);
            }
            tail.push(entry.clone());
        }
        let _ = ch.tx.send(entry);
    }

    /// Subscribe to future lines for this agent and return a snapshot of
    /// the last N buffered lines.
    pub async fn subscribe(
        &self,
        agent_id: &str,
    ) -> (broadcast::Receiver<PiTerminalLine>, Vec<PiTerminalLine>) {
        let ch = self.channel(agent_id).await;
        let rx = ch.tx.subscribe();
        let tail = ch.tail.read().await.clone();
        (rx, tail)
    }
}
