use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// Webhook connector.
///
/// **Source**: Incoming webhooks are handled by the API route
/// `POST /api/connectors/webhook/{channel_id}`, which sends events directly
/// through the stored `event_tx`. The connector itself just holds the sender.
///
/// **Sink**: Outgoing webhooks POST rendered templates to a configured URL.
pub struct WebhookConnector {
    client: reqwest::Client,
    event_tx: Option<mpsc::Sender<ConnectorEvent>>,
    channels: Vec<ChannelConfig>,
    config: Value,
}

impl WebhookConnector {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            event_tx: None,
            channels: Vec::new(),
            config: Value::Null,
        }
    }

    /// Get the event sender for use by the API route.
    pub fn event_tx(&self) -> Option<&mpsc::Sender<ConnectorEvent>> {
        self.event_tx.as_ref()
    }
}

#[async_trait]
impl Connector for WebhookConnector {
    fn connector_type(&self) -> &str {
        "webhook"
    }

    async fn validate_config(&self, config: &Value) -> ValidationResult {
        // For sink channels, a URL must be configured at the connector or channel level.
        // Source channels don't require any specific config (they receive via API route).
        // We check for basic structure here; channel-level URL is optional at connector level.
        if let Some(obj) = config.as_object() {
            // If a url is specified, it should be a non-empty string
            if let Some(url) = obj.get("url") {
                if let Some(url_str) = url.as_str() {
                    if url_str.is_empty() {
                        return ValidationResult {
                            valid: false,
                            error: Some("url must not be empty".to_string()),
                        };
                    }
                }
            }
        }

        ValidationResult {
            valid: true,
            error: None,
        }
    }

    async fn start(
        &mut self,
        config: &Value,
        channels: &[ChannelConfig],
        event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        info!(channels = channels.len(), "webhook connector started");

        self.event_tx = Some(event_tx);
        self.channels = channels.to_vec();
        self.config = config.clone();
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("webhook connector stopped");
        self.event_tx = None;
        self.channels.clear();
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        // Find the channel config to get the URL
        let channel = self.channels.iter().find(|ch| ch.id == message.channel_id);

        // Resolve the target URL: channel config overrides connector config
        let url = channel
            .and_then(|ch| ch.config.get("url").and_then(|v| v.as_str()))
            .or_else(|| self.config.get("url").and_then(|v| v.as_str()));

        let url = match url {
            Some(u) => u,
            None => {
                warn!(
                    channel_id = message.channel_id.as_str(),
                    "no url configured for webhook sink"
                );
                return Err(Error::Connector(
                    "no url configured for webhook sink channel".to_string(),
                ));
            }
        };

        let body = render_template(&message.template, &message.context);

        debug!(
            url = url,
            channel_id = message.channel_id.as_str(),
            "sending webhook"
        );

        let resp = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body.clone())
            .send()
            .await
            .map_err(|e| Error::Connector(format!("webhook POST failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            error!(
                url = url,
                status = %status,
                response = body.as_str(),
                "webhook POST returned error"
            );
            return Err(Error::Connector(format!("webhook POST returned {status}")));
        }

        info!(
            url = url,
            channel_id = message.channel_id.as_str(),
            "webhook sent successfully"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        // Webhook connector is healthy as long as it has an event_tx
        self.event_tx.is_some()
    }
}

/// Simple template renderer: replaces `{{key}}` with values from context.
fn render_template(template: &str, context: &Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = context.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{}}}}}", key);
            let replacement = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}
