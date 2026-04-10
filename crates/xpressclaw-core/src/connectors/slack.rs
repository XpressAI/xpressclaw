use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// Slack Bot API connector.
///
/// **Source**: Polls `conversations.history` for new messages.
/// **Sink**: Sends messages via `chat.postMessage`.
pub struct SlackConnector {
    client: reqwest::Client,
    bot_token: Option<String>,
    channels: Vec<ChannelConfig>,
    connector_id: String,
    shutdown: Arc<AtomicBool>,
    poll_handle: Option<JoinHandle<()>>,
}

impl Default for SlackConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl SlackConnector {
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
impl Connector for SlackConnector {
    fn connector_type(&self) -> &str {
        "slack"
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

        if !bot_token.starts_with("xoxb-") {
            return ValidationResult {
                valid: false,
                error: Some("bot_token must start with 'xoxb-'".to_string()),
            };
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
        let bot_token = config
            .get("bot_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("bot_token is required".to_string()))?
            .to_string();

        self.bot_token = Some(bot_token.clone());
        self.channels = channels.to_vec();
        self.shutdown.store(false, Ordering::SeqCst);

        let connector_id = channels.first().map(|ch| ch.id.clone()).unwrap_or_default();
        self.connector_id = connector_id.clone();

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
                poll_slack_messages(client, token, source_channels, event_tx, shutdown).await;
            });

            self.poll_handle = Some(handle);
        }

        info!(
            bot_token_prefix = &bot_token[..bot_token.len().min(10)],
            channels = channels.len(),
            "slack connector started"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("slack connector stopping");
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.poll_handle.take() {
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
            .ok_or_else(|| Error::Connector("slack connector not started".to_string()))?;

        let channel = self.channels.iter().find(|ch| ch.id == message.channel_id);

        let channel_id = channel
            .and_then(|ch| ch.config.get("channel_id"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Connector(format!(
                    "no channel_id configured for channel {}",
                    message.channel_id
                ))
            })?;

        let text = render_template(&message.template, &message.context);

        debug!(
            channel_id = channel_id,
            sink_channel = message.channel_id.as_str(),
            "sending slack message"
        );

        let resp = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {}", bot_token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&json!({
                "channel": channel_id,
                "text": text,
            }))
            .send()
            .await
            .map_err(|e| Error::Connector(format!("slack chat.postMessage failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            error!(
                status = %status,
                response = resp_body.as_str(),
                "slack chat.postMessage error"
            );
            return Err(Error::Connector(format!(
                "slack chat.postMessage returned {status}: {resp_body}"
            )));
        }

        // Slack returns 200 even on API errors — check the "ok" field
        let body: Value = resp
            .json()
            .await
            .map_err(|e| Error::Connector(format!("failed to parse slack response: {e}")))?;

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(Error::Connector(format!(
                "slack chat.postMessage API error: {err}"
            )));
        }

        info!(
            channel_id = channel_id,
            sink_channel = message.channel_id.as_str(),
            "slack message sent"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        if let Some(token) = &self.bot_token {
            match self
                .client
                .post("https://slack.com/api/auth.test")
                .header("Authorization", format!("Bearer {}", token))
                .header("Content-Type", "application/json")
                .send()
                .await
            {
                Ok(resp) => {
                    if let Ok(body) = resp.json::<Value>().await {
                        body.get("ok").and_then(|v| v.as_bool()) == Some(true)
                    } else {
                        false
                    }
                }
                Err(_) => false,
            }
        } else {
            false
        }
    }
}

/// Polling loop for Slack conversations.history.
async fn poll_slack_messages(
    client: reqwest::Client,
    bot_token: String,
    source_channels: Vec<ChannelConfig>,
    event_tx: mpsc::Sender<ConnectorEvent>,
    shutdown: Arc<AtomicBool>,
) {
    // Track the latest timestamp per Slack channel to avoid re-processing.
    // Key: Slack channel_id, Value: latest message ts
    let mut latest_ts: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Get our own bot_id so we can filter out our own messages
    let bot_id = fetch_bot_id(&client, &bot_token).await;

    info!(
        bot_id = bot_id.as_deref().unwrap_or("unknown"),
        "slack poll loop started"
    );

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("slack poll loop shutting down");
            break;
        }

        for channel in &source_channels {
            let slack_channel_id = match channel.config.get("channel_id").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => {
                    warn!(
                        channel_id = channel.id.as_str(),
                        "slack source channel missing channel_id in config, skipping"
                    );
                    continue;
                }
            };

            let mut params = vec![
                ("channel".to_string(), slack_channel_id.to_string()),
                ("limit".to_string(), "20".to_string()),
            ];

            if let Some(ts) = latest_ts.get(slack_channel_id) {
                params.push(("oldest".to_string(), ts.clone()));
            }

            let resp = client
                .get("https://slack.com/api/conversations.history")
                .header("Authorization", format!("Bearer {}", bot_token))
                .query(&params)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, channel = slack_channel_id, "slack conversations.history request failed");
                    continue;
                }
            };

            let body: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "failed to parse slack conversations.history response");
                    continue;
                }
            };

            if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                let err = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                warn!(
                    error = err,
                    channel = slack_channel_id,
                    "slack conversations.history returned error"
                );

                // Handle rate limiting
                if err == "ratelimited" {
                    let retry_after = body
                        .get("headers")
                        .and_then(|h| h.get("Retry-After"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(30);
                    warn!(retry_after, "slack rate limited, backing off");
                    tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
                }
                continue;
            }

            let messages = match body.get("messages").and_then(|v| v.as_array()) {
                Some(arr) => arr,
                None => continue,
            };

            // Messages come newest-first; process oldest-first
            let mut sorted_messages = messages.clone();
            sorted_messages.sort_by(|a, b| {
                let ts_a = a.get("ts").and_then(|v| v.as_str()).unwrap_or("0");
                let ts_b = b.get("ts").and_then(|v| v.as_str()).unwrap_or("0");
                ts_a.partial_cmp(ts_b).unwrap_or(std::cmp::Ordering::Equal)
            });

            for msg in &sorted_messages {
                let ts = match msg.get("ts").and_then(|v| v.as_str()) {
                    Some(t) => t,
                    None => continue,
                };

                // Skip messages we've already seen (oldest param is exclusive
                // but we store the latest ts we processed, not the next one)
                if let Some(prev_ts) = latest_ts.get(slack_channel_id) {
                    if ts <= prev_ts.as_str() {
                        continue;
                    }
                }

                // Filter out bot's own messages
                if let Some(ref our_bot_id) = bot_id {
                    if let Some(msg_bot_id) = msg.get("bot_id").and_then(|v| v.as_str()) {
                        if msg_bot_id == our_bot_id {
                            latest_ts.insert(slack_channel_id.to_string(), ts.to_string());
                            continue;
                        }
                    }
                }

                let user = msg
                    .get("user")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let text = msg
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let payload = json!({
                    "channel_id": slack_channel_id,
                    "user": user,
                    "text": text,
                    "ts": ts,
                });

                let event = ConnectorEvent {
                    connector_id: channel.id.clone(),
                    channel_id: channel.id.clone(),
                    event_type: "message".to_string(),
                    payload,
                };

                if let Err(e) = event_tx.send(event).await {
                    error!(error = %e, "failed to send slack event");
                    break;
                }

                debug!(
                    channel = slack_channel_id,
                    user = user.as_str(),
                    ts = ts,
                    "processed slack message"
                );

                latest_ts.insert(slack_channel_id.to_string(), ts.to_string());
            }
        }

        // Poll every 5 seconds
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Fetch our bot's bot_id so we can filter out self-posted messages.
async fn fetch_bot_id(client: &reqwest::Client, bot_token: &str) -> Option<String> {
    let resp = client
        .post("https://slack.com/api/auth.test")
        .header("Authorization", format!("Bearer {}", bot_token))
        .header("Content-Type", "application/json")
        .send()
        .await
        .ok()?;

    let body: Value = resp.json().await.ok()?;
    if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }

    body.get("bot_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
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
