use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// Telegram Bot API connector.
///
/// **Source**: Uses long polling via `getUpdates` to receive messages.
/// **Sink**: Sends messages via `sendMessage`.
pub struct TelegramConnector {
    client: reqwest::Client,
    bot_token: Option<String>,
    channels: Vec<ChannelConfig>,
    connector_id: String,
    shutdown: Arc<AtomicBool>,
    poll_handle: Option<JoinHandle<()>>,
}

impl TelegramConnector {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            bot_token: None,
            channels: Vec::new(),
            connector_id: String::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            poll_handle: None,
        }
    }
}

#[async_trait]
impl Connector for TelegramConnector {
    fn connector_type(&self) -> &str {
        "telegram"
    }

    async fn validate_config(&self, config: &Value) -> ValidationResult {
        let bot_token = config
            .get("bot_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if bot_token.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("bot_token is required".to_string()),
            };
        }

        // Optionally verify the token by calling getMe
        let url = format!("https://api.telegram.org/bot{}/getMe", bot_token);
        match reqwest::Client::new().get(&url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    ValidationResult {
                        valid: true,
                        error: None,
                    }
                } else {
                    ValidationResult {
                        valid: false,
                        error: Some(format!("Telegram API returned {} for getMe", resp.status())),
                    }
                }
            }
            Err(e) => {
                // Network errors during validation are non-fatal — the token
                // format might still be correct, just can't reach Telegram right now.
                warn!(error = %e, "could not verify Telegram bot token, allowing anyway");
                ValidationResult {
                    valid: true,
                    error: None,
                }
            }
        }
    }

    async fn start(
        &mut self,
        config: &Value,
        channels: &[ChannelConfig],
        event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        let bot_token = config
            .get("bot_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("bot_token is required".to_string()))?
            .to_string();

        self.bot_token = Some(bot_token.clone());
        self.channels = channels.to_vec();
        self.shutdown.store(false, Ordering::SeqCst);

        // Determine the connector_id from the first channel, or use a placeholder
        let connector_id = channels.first().map(|ch| ch.id.clone()).unwrap_or_default();
        self.connector_id = connector_id.clone();

        // Spawn the long-polling task for source channels
        let has_source = channels
            .iter()
            .any(|ch| ch.channel_type == "source" || ch.channel_type == "both");

        if has_source {
            let client = self.client.clone();
            let token = bot_token.clone();
            let shutdown = self.shutdown.clone();
            let source_channels: Vec<ChannelConfig> = channels
                .iter()
                .filter(|ch| ch.channel_type == "source" || ch.channel_type == "both")
                .cloned()
                .collect();

            let handle = tokio::spawn(async move {
                poll_updates(client, token, source_channels, event_tx, shutdown).await;
            });

            self.poll_handle = Some(handle);
        }

        info!(
            bot_token_prefix = &bot_token[..bot_token.len().min(10)],
            channels = channels.len(),
            "telegram connector started"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("telegram connector stopping");
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.poll_handle.take() {
            // Give the poll loop a moment to notice the shutdown signal
            handle.abort();
            let _ = handle.await;
        }

        self.bot_token = None;
        self.channels.clear();
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        let bot_token = self
            .bot_token
            .as_deref()
            .ok_or_else(|| Error::Connector("telegram connector not started".to_string()))?;

        // Resolve chat_id from channel config
        let channel = self.channels.iter().find(|ch| ch.id == message.channel_id);

        let chat_id = channel
            .and_then(|ch| ch.config.get("chat_id"))
            .ok_or_else(|| {
                Error::Connector(format!(
                    "no chat_id configured for channel {}",
                    message.channel_id
                ))
            })?;

        let text = render_template(&message.template, &message.context);

        let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
        let body = json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        debug!(
            chat_id = %chat_id,
            channel_id = message.channel_id.as_str(),
            "sending telegram message"
        );

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Connector(format!("telegram sendMessage failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            error!(
                status = %status,
                response = resp_body.as_str(),
                "telegram sendMessage error"
            );
            return Err(Error::Connector(format!(
                "telegram sendMessage returned {status}: {resp_body}"
            )));
        }

        info!(
            chat_id = %chat_id,
            channel_id = message.channel_id.as_str(),
            "telegram message sent"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        if let Some(token) = &self.bot_token {
            let url = format!("https://api.telegram.org/bot{}/getMe", token);
            match self.client.get(&url).send().await {
                Ok(resp) => resp.status().is_success(),
                Err(_) => false,
            }
        } else {
            false
        }
    }
}

/// Long-polling loop for Telegram getUpdates.
async fn poll_updates(
    client: reqwest::Client,
    bot_token: String,
    source_channels: Vec<ChannelConfig>,
    event_tx: mpsc::Sender<ConnectorEvent>,
    shutdown: Arc<AtomicBool>,
) {
    let mut offset: i64 = 0;
    let url = format!("https://api.telegram.org/bot{}/getUpdates", bot_token);

    info!("telegram poll loop started");

    loop {
        if shutdown.load(Ordering::SeqCst) {
            info!("telegram poll loop shutting down");
            break;
        }

        let resp = client
            .get(&url)
            .query(&[
                ("offset", offset.to_string()),
                ("timeout", "30".to_string()),
            ])
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "telegram getUpdates request failed, retrying in 5s");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "failed to parse telegram getUpdates response");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            warn!(response = %body, "telegram getUpdates returned ok=false");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            continue;
        }

        let results = match body.get("result").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => continue,
        };

        for update in results {
            let update_id = update
                .get("update_id")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            if update_id >= offset {
                offset = update_id + 1;
            }

            // Only handle message updates
            let message = match update.get("message") {
                Some(m) => m,
                None => continue,
            };

            let chat_id = message.pointer("/chat/id").cloned().unwrap_or(Value::Null);
            let from = message.get("from").cloned().unwrap_or(Value::Null);
            let text = message
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let message_id = message.get("message_id").cloned().unwrap_or(Value::Null);

            let payload = json!({
                "chat_id": chat_id,
                "from": {
                    "id": from.get("id").cloned().unwrap_or(Value::Null),
                    "first_name": from.get("first_name").cloned().unwrap_or(Value::Null),
                    "username": from.get("username").cloned().unwrap_or(Value::Null),
                },
                "text": text,
                "message_id": message_id,
            });

            // Send event for each source channel
            for channel in &source_channels {
                let event = ConnectorEvent {
                    connector_id: channel.id.clone(),
                    channel_id: channel.id.clone(),
                    event_type: "telegram.message".to_string(),
                    payload: payload.clone(),
                };

                if let Err(e) = event_tx.send(event).await {
                    error!(error = %e, "failed to send telegram event");
                    break;
                }
            }

            debug!(
                chat_id = %chat_id,
                text = text.as_str(),
                "processed telegram update"
            );
        }
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
