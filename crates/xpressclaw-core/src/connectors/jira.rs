use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// Jira Cloud API connector.
///
/// **Source**: Polls JQL search for recently updated issues.
/// **Sink**: Adds comments to Jira issues using the Atlassian Document Format.
pub struct JiraConnector {
    client: reqwest::Client,
    base_url: Option<String>,
    email: Option<String>,
    api_token: Option<String>,
    channels: Vec<ChannelConfig>,
    connector_id: String,
    shutdown: Arc<AtomicBool>,
    poll_handle: Option<JoinHandle<()>>,
}

impl Default for JiraConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl JiraConnector {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: None,
            email: None,
            api_token: None,
            channels: Vec::new(),
            connector_id: String::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            poll_handle: None,
        }
    }

    fn auth_header_value(email: &str, api_token: &str) -> String {
        let credentials = format!("{}:{}", email, api_token);
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes());
        format!("Basic {}", encoded)
    }
}

#[async_trait]
impl Connector for JiraConnector {
    fn connector_type(&self) -> &str {
        "jira"
    }

    async fn validate_config(&self, config: &Value) -> ValidationResult {
        let base_url = config
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let email = config.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let api_token = config
            .get("api_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if base_url.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("base_url is required".to_string()),
            };
        }

        if email.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("email is required".to_string()),
            };
        }

        if api_token.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("api_token is required".to_string()),
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
        let base_url = config
            .get("base_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("base_url is required".to_string()))?
            .trim_end_matches('/')
            .to_string();
        let email = config
            .get("email")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("email is required".to_string()))?
            .to_string();
        let api_token = config
            .get("api_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("api_token is required".to_string()))?
            .to_string();

        self.base_url = Some(base_url.clone());
        self.email = Some(email.clone());
        self.api_token = Some(api_token.clone());
        self.channels = channels.to_vec();
        self.shutdown.store(false, Ordering::SeqCst);

        let connector_id = channels.first().map(|ch| ch.id.clone()).unwrap_or_default();
        self.connector_id = connector_id.clone();

        let has_source = channels
            .iter()
            .any(|ch| ch.channel_type == "source" || ch.channel_type == "both");

        if has_source {
            let client = self.client.clone();
            let shutdown = self.shutdown.clone();
            let source_channels: Vec<ChannelConfig> = channels
                .iter()
                .filter(|ch| ch.channel_type == "source" || ch.channel_type == "both")
                .cloned()
                .collect();

            let bu = base_url.clone();
            let em = email.clone();
            let at = api_token.clone();

            let handle = tokio::spawn(async move {
                poll_jira_issues(client, bu, em, at, source_channels, event_tx, shutdown).await;
            });

            self.poll_handle = Some(handle);
        }

        info!(
            base_url = base_url.as_str(),
            channels = channels.len(),
            "jira connector started"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("jira connector stopping");
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.poll_handle.take() {
            handle.abort();
            let _ = handle.await;
        }

        self.base_url = None;
        self.email = None;
        self.api_token = None;
        self.channels.clear();
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        let base_url = self
            .base_url
            .as_deref()
            .ok_or_else(|| Error::Connector("jira connector not started".to_string()))?;
        let email = self
            .email
            .as_deref()
            .ok_or_else(|| Error::Connector("jira connector not started".to_string()))?;
        let api_token = self
            .api_token
            .as_deref()
            .ok_or_else(|| Error::Connector("jira connector not started".to_string()))?;

        let text = render_template(&message.template, &message.context);

        // The issue key can come from the message context
        let issue_key = message
            .context
            .get("issue_key")
            .or_else(|| message.context.get("key"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Connector(
                    "issue_key or key required in message context for jira sink".to_string(),
                )
            })?;

        let url = format!("{base_url}/rest/api/3/issue/{issue_key}/comment");

        // Atlassian Document Format (ADF) body
        let body = json!({
            "body": {
                "type": "doc",
                "version": 1,
                "content": [{
                    "type": "paragraph",
                    "content": [{
                        "type": "text",
                        "text": text
                    }]
                }]
            }
        });

        let auth = Self::auth_header_value(email, api_token);

        debug!(issue_key = issue_key, "posting jira comment");

        let resp = self
            .client
            .post(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Connector(format!("jira comment POST failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            error!(
                status = %status,
                response = resp_body.as_str(),
                "jira comment POST error"
            );
            return Err(Error::Connector(format!(
                "jira comment POST returned {status}: {resp_body}"
            )));
        }

        info!(
            issue_key = issue_key,
            channel_id = message.channel_id.as_str(),
            "jira comment posted"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        if let (Some(base_url), Some(email), Some(api_token)) =
            (&self.base_url, &self.email, &self.api_token)
        {
            let url = format!("{base_url}/rest/api/3/myself");
            let auth = Self::auth_header_value(email, api_token);

            match self
                .client
                .get(&url)
                .header("Authorization", &auth)
                .send()
                .await
            {
                Ok(resp) => resp.status().is_success(),
                Err(_) => false,
            }
        } else {
            false
        }
    }
}

/// Polling loop for Jira issue updates via JQL search.
async fn poll_jira_issues(
    client: reqwest::Client,
    base_url: String,
    email: String,
    api_token: String,
    source_channels: Vec<ChannelConfig>,
    event_tx: mpsc::Sender<ConnectorEvent>,
    shutdown: Arc<AtomicBool>,
) {
    let auth = JiraConnector::auth_header_value(&email, &api_token);

    // Track the last updated timestamp per channel to avoid re-processing
    let mut last_updated: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    info!("jira poll loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("jira poll loop shutting down");
            break;
        }

        for channel in &source_channels {
            let jql = channel
                .config
                .get("jql")
                .and_then(|v| v.as_str())
                .unwrap_or("updated > -5m");

            let url = format!("{}/rest/api/3/search", base_url);

            let resp = client
                .get(&url)
                .header("Authorization", &auth)
                .header("Accept", "application/json")
                .query(&[
                    ("jql", jql),
                    ("maxResults", "10"),
                    ("fields", "summary,status,description,assignee,updated"),
                ])
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "jira search request failed, retrying");
                    continue;
                }
            };

            let status = resp.status();

            // Handle rate limiting (Jira returns 429)
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(60);
                warn!(retry_after, "jira rate limited, backing off");
                tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
                continue;
            }

            if !status.is_success() {
                warn!(status = %status, "jira search returned error");
                continue;
            }

            let body: Value = match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "failed to parse jira search response");
                    continue;
                }
            };

            let issues = match body.get("issues").and_then(|v| v.as_array()) {
                Some(arr) => arr,
                None => continue,
            };

            for issue in issues {
                let key = issue
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let updated = issue
                    .pointer("/fields/updated")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Skip if we've already seen this update
                let track_key = format!("{}:{}", channel.id, key);
                if let Some(prev) = last_updated.get(&track_key) {
                    if updated <= *prev {
                        continue;
                    }
                }

                let summary = issue
                    .pointer("/fields/summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let status_name = issue
                    .pointer("/fields/status/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let description = issue
                    .pointer("/fields/description")
                    .cloned()
                    .unwrap_or(Value::Null);

                // Extract plain text from ADF description if possible
                let description_text = extract_adf_text(&description);

                let assignee = issue
                    .pointer("/fields/assignee/displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let payload = json!({
                    "key": key,
                    "summary": summary,
                    "status": status_name,
                    "description": description_text,
                    "assignee": assignee,
                    "updated": updated,
                });

                let event = ConnectorEvent {
                    connector_id: channel.id.clone(),
                    channel_id: channel.id.clone(),
                    event_type: "issue_updated".to_string(),
                    payload,
                };

                if let Err(e) = event_tx.send(event).await {
                    error!(error = %e, "failed to send jira event");
                    break;
                }

                debug!(
                    key = key.as_str(),
                    summary = summary.as_str(),
                    "processed jira issue update"
                );

                last_updated.insert(track_key, updated);
            }
        }

        // Poll every 30 seconds
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

/// Extract plain text from an Atlassian Document Format (ADF) value.
/// Falls back to an empty string if the structure is unexpected.
fn extract_adf_text(adf: &Value) -> String {
    if let Some(content) = adf.get("content").and_then(|v| v.as_array()) {
        let mut parts = Vec::new();
        for block in content {
            if let Some(inner) = block.get("content").and_then(|v| v.as_array()) {
                for node in inner {
                    if let Some(text) = node.get("text").and_then(|v| v.as_str()) {
                        parts.push(text.to_string());
                    }
                }
            }
        }
        parts.join("\n")
    } else if let Some(s) = adf.as_str() {
        s.to_string()
    } else {
        String::new()
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
