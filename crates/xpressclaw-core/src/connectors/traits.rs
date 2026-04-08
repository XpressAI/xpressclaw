use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// An event produced by a connector source channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorEvent {
    pub connector_id: String,
    pub channel_id: String,
    pub event_type: String,
    pub payload: Value,
}

/// A message to be sent through a connector sink channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkMessage {
    pub channel_id: String,
    pub template: String,
    pub context: Value,
}

/// Configuration for a single channel within a connector.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: String,
    pub name: String,
    pub channel_type: String, // source, sink, both
    pub config: Value,
    pub agent_id: Option<String>, // direct binding
}

/// Result of validating a connector configuration.
pub struct ValidationResult {
    pub valid: bool,
    pub error: Option<String>,
}

/// The core trait that all connector implementations must satisfy.
///
/// Connectors bridge external systems (webhooks, Telegram, file watchers, etc.)
/// into the xpressclaw event system. Each connector can act as a source
/// (producing events), a sink (sending messages), or both.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Returns the type identifier for this connector (e.g. "webhook", "telegram").
    fn connector_type(&self) -> &str;

    /// Validate the given configuration before starting.
    async fn validate_config(&self, config: &Value) -> ValidationResult;

    /// Start the connector with the given configuration and channels.
    ///
    /// Source channels should emit events through the provided `event_tx` sender.
    async fn start(
        &mut self,
        config: &Value,
        channels: &[ChannelConfig],
        event_tx: tokio::sync::mpsc::Sender<ConnectorEvent>,
    ) -> Result<()>;

    /// Stop the connector, cleaning up any background tasks or resources.
    async fn stop(&mut self) -> Result<()>;

    /// Send a message through a sink channel.
    async fn send(&self, message: &SinkMessage) -> Result<()>;

    /// Check whether the connector is healthy and operational.
    async fn health(&self) -> bool;
}
