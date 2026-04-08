use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::info;

use crate::error::Result;

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

// ---------------------------------------------------------------------------
// Email stub
// ---------------------------------------------------------------------------

pub struct EmailConnector;

#[async_trait]
impl Connector for EmailConnector {
    fn connector_type(&self) -> &str {
        "email"
    }

    async fn validate_config(&self, _config: &Value) -> ValidationResult {
        info!("email connector: validate_config (stub)");
        ValidationResult {
            valid: true,
            error: None,
        }
    }

    async fn start(
        &mut self,
        _config: &Value,
        _channels: &[ChannelConfig],
        _event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        info!("email connector: started (stub)");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("email connector: stopped (stub)");
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        info!(
            channel_id = message.channel_id.as_str(),
            template = message.template.as_str(),
            "email connector: send (stub)"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// GitHub stub
// ---------------------------------------------------------------------------

pub struct GitHubConnector;

#[async_trait]
impl Connector for GitHubConnector {
    fn connector_type(&self) -> &str {
        "github"
    }

    async fn validate_config(&self, _config: &Value) -> ValidationResult {
        info!("github connector: validate_config (stub)");
        ValidationResult {
            valid: true,
            error: None,
        }
    }

    async fn start(
        &mut self,
        _config: &Value,
        _channels: &[ChannelConfig],
        _event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        info!("github connector: started (stub)");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("github connector: stopped (stub)");
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        info!(
            channel_id = message.channel_id.as_str(),
            template = message.template.as_str(),
            "github connector: send (stub)"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Jira stub
// ---------------------------------------------------------------------------

pub struct JiraConnector;

#[async_trait]
impl Connector for JiraConnector {
    fn connector_type(&self) -> &str {
        "jira"
    }

    async fn validate_config(&self, _config: &Value) -> ValidationResult {
        info!("jira connector: validate_config (stub)");
        ValidationResult {
            valid: true,
            error: None,
        }
    }

    async fn start(
        &mut self,
        _config: &Value,
        _channels: &[ChannelConfig],
        _event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        info!("jira connector: started (stub)");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("jira connector: stopped (stub)");
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        info!(
            channel_id = message.channel_id.as_str(),
            template = message.template.as_str(),
            "jira connector: send (stub)"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Slack stub
// ---------------------------------------------------------------------------

pub struct SlackConnector;

#[async_trait]
impl Connector for SlackConnector {
    fn connector_type(&self) -> &str {
        "slack"
    }

    async fn validate_config(&self, _config: &Value) -> ValidationResult {
        info!("slack connector: validate_config (stub)");
        ValidationResult {
            valid: true,
            error: None,
        }
    }

    async fn start(
        &mut self,
        _config: &Value,
        _channels: &[ChannelConfig],
        _event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        info!("slack connector: started (stub)");
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("slack connector: stopped (stub)");
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        info!(
            channel_id = message.channel_id.as_str(),
            template = message.template.as_str(),
            "slack connector: send (stub)"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        true
    }
}
