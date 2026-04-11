use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// GitHub API connector.
///
/// **Source**: Polls repository events via the Events API.
/// **Sink**: Creates issues or posts comments on issues/PRs.
pub struct GitHubConnector {
    client: reqwest::Client,
    token: Option<String>,
    owner: Option<String>,
    repo: Option<String>,
    channels: Vec<ChannelConfig>,
    connector_id: String,
    shutdown: Arc<AtomicBool>,
    poll_handle: Option<JoinHandle<()>>,
}

impl Default for GitHubConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubConnector {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .user_agent("xpressclaw/0.1")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            token: None,
            owner: None,
            repo: None,
            channels: Vec::new(),
            connector_id: String::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            poll_handle: None,
        }
    }
}

#[async_trait]
impl Connector for GitHubConnector {
    fn connector_type(&self) -> &str {
        "github"
    }

    async fn validate_config(&self, config: &Value) -> ValidationResult {
        let token = config.get("token").and_then(|v| v.as_str()).unwrap_or("");
        let owner = config.get("owner").and_then(|v| v.as_str()).unwrap_or("");
        let repo = config.get("repo").and_then(|v| v.as_str()).unwrap_or("");

        if token.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("token is required".to_string()),
            };
        }

        if owner.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("owner is required".to_string()),
            };
        }

        if repo.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("repo is required".to_string()),
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
        let token = config
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("token is required".to_string()))?
            .to_string();
        let owner = config
            .get("owner")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("owner is required".to_string()))?
            .to_string();
        let repo = config
            .get("repo")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("repo is required".to_string()))?
            .to_string();

        self.token = Some(token.clone());
        self.owner = Some(owner.clone());
        self.repo = Some(repo.clone());
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

            let t = token.clone();
            let o = owner.clone();
            let r = repo.clone();

            let handle = tokio::spawn(async move {
                poll_github_events(client, t, o, r, source_channels, event_tx, shutdown).await;
            });

            self.poll_handle = Some(handle);
        }

        info!(
            owner = owner.as_str(),
            repo = repo.as_str(),
            channels = channels.len(),
            "github connector started"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("github connector stopping");
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.poll_handle.take() {
            handle.abort();
            let _ = handle.await;
        }

        self.token = None;
        self.owner = None;
        self.repo = None;
        self.channels.clear();
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        let token = self
            .token
            .as_deref()
            .ok_or_else(|| Error::Connector("github connector not started".to_string()))?;
        let owner = self
            .owner
            .as_deref()
            .ok_or_else(|| Error::Connector("github connector not started".to_string()))?;
        let repo = self
            .repo
            .as_deref()
            .ok_or_else(|| Error::Connector("github connector not started".to_string()))?;

        let channel = self.channels.iter().find(|ch| ch.id == message.channel_id);

        let text = render_template(&message.template, &message.context);

        // Determine action from context: create_issue or comment on existing issue
        let issue_number = message.context.get("issue_number").and_then(|v| v.as_u64());

        if let Some(num) = issue_number {
            // Comment on existing issue/PR
            let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{num}/comments");

            debug!(issue_number = num, "posting github comment");

            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("token {}", token))
                .header("Accept", "application/vnd.github.v3+json")
                .json(&json!({ "body": text }))
                .send()
                .await
                .map_err(|e| Error::Connector(format!("github comment POST failed: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let resp_body = resp.text().await.unwrap_or_default();
                error!(
                    status = %status,
                    response = resp_body.as_str(),
                    "github comment POST error"
                );
                return Err(Error::Connector(format!(
                    "github comment POST returned {status}: {resp_body}"
                )));
            }

            info!(
                issue_number = num,
                channel_id = message.channel_id.as_str(),
                "github comment posted"
            );
        } else {
            // Create a new issue
            let url = format!("https://api.github.com/repos/{owner}/{repo}/issues");

            let title = message
                .context
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("New issue from xpressclaw");

            let labels: Vec<String> = message
                .context
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            debug!(title = title, "creating github issue");

            let mut body_json = json!({
                "title": title,
                "body": text,
            });

            if !labels.is_empty() {
                body_json["labels"] = json!(labels);
            }

            // If channel config specifies assignees, include them
            if let Some(ch) = channel {
                if let Some(assignees) = ch.config.get("assignees") {
                    body_json["assignees"] = assignees.clone();
                }
            }

            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("token {}", token))
                .header("Accept", "application/vnd.github.v3+json")
                .json(&body_json)
                .send()
                .await
                .map_err(|e| Error::Connector(format!("github create issue failed: {e}")))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let resp_body = resp.text().await.unwrap_or_default();
                error!(
                    status = %status,
                    response = resp_body.as_str(),
                    "github create issue error"
                );
                return Err(Error::Connector(format!(
                    "github create issue returned {status}: {resp_body}"
                )));
            }

            info!(
                channel_id = message.channel_id.as_str(),
                "github issue created"
            );
        }

        Ok(())
    }

    async fn health(&self) -> bool {
        if let (Some(token), Some(owner), Some(repo)) = (&self.token, &self.owner, &self.repo) {
            let url = format!("https://api.github.com/repos/{owner}/{repo}");
            match self
                .client
                .get(&url)
                .header("Authorization", format!("token {}", token))
                .header("Accept", "application/vnd.github.v3+json")
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

/// Polling loop for GitHub repository events.
async fn poll_github_events(
    client: reqwest::Client,
    token: String,
    owner: String,
    repo: String,
    source_channels: Vec<ChannelConfig>,
    event_tx: mpsc::Sender<ConnectorEvent>,
    shutdown: Arc<AtomicBool>,
) {
    let url = format!("https://api.github.com/repos/{owner}/{repo}/events");

    let mut etag: Option<String> = None;
    let mut last_event_id: Option<String> = None;
    let mut poll_interval_secs: u64 = 30;

    info!(
        owner = owner.as_str(),
        repo = repo.as_str(),
        "github poll loop started"
    );

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("github poll loop shutting down");
            break;
        }

        let mut req = client
            .get(&url)
            .header("Authorization", format!("token {}", token))
            .header("Accept", "application/vnd.github.v3+json")
            .query(&[("per_page", "30")]);

        if let Some(ref etag_val) = etag {
            req = req.header("If-None-Match", etag_val.as_str());
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "github events request failed, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
                continue;
            }
        };

        // Respect rate limit headers
        if let Some(remaining) = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            if remaining < 10 {
                warn!(remaining, "github rate limit low, increasing poll interval");
                poll_interval_secs = 60;
            } else {
                poll_interval_secs = 30;
            }
        }

        // Update the poll interval from the X-Poll-Interval header if present
        if let Some(interval) = resp
            .headers()
            .get("x-poll-interval")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
        {
            poll_interval_secs = poll_interval_secs.max(interval);
        }

        // Store new ETag
        if let Some(new_etag) = resp.headers().get("etag").and_then(|v| v.to_str().ok()) {
            etag = Some(new_etag.to_string());
        }

        let status = resp.status();

        if status == reqwest::StatusCode::NOT_MODIFIED {
            debug!("github events: no new events (304)");
            tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
            continue;
        }

        if status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            warn!(status = %status, "github rate limited, backing off for 60s");
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            continue;
        }

        if !status.is_success() {
            warn!(status = %status, "github events returned error");
            tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
            continue;
        }

        let events: Vec<Value> = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "failed to parse github events response");
                tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
                continue;
            }
        };

        // Events are returned newest-first; process oldest-first
        let mut new_events: Vec<&Value> = Vec::new();
        for event in events.iter().rev() {
            let event_id = event.get("id").and_then(|v| v.as_str());

            // Skip events we've already seen
            if let (Some(eid), Some(ref lid)) = (event_id, &last_event_id) {
                if eid <= lid.as_str() {
                    continue;
                }
            }

            new_events.push(event);
        }

        for event in &new_events {
            let event_type = event
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let event_id = event
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Filter events by channel config type
            for channel in &source_channels {
                let channel_type = channel
                    .config
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("all");

                let matches = match channel_type {
                    "issues" => matches!(event_type.as_str(), "IssuesEvent" | "IssueCommentEvent"),
                    "pull_requests" => matches!(
                        event_type.as_str(),
                        "PullRequestEvent"
                            | "PullRequestReviewEvent"
                            | "PullRequestReviewCommentEvent"
                    ),
                    "notifications" => true, // all events
                    "all" => true,
                    _ => true,
                };

                if !matches {
                    continue;
                }

                let connector_event = ConnectorEvent {
                    connector_id: channel.id.clone(),
                    channel_id: channel.id.clone(),
                    event_type: event_type.clone(),
                    payload: (*event).clone(),
                };

                if let Err(e) = event_tx.send(connector_event).await {
                    error!(error = %e, "failed to send github event");
                    break;
                }

                debug!(
                    event_type = event_type.as_str(),
                    event_id = event_id.as_str(),
                    "processed github event"
                );
            }

            if !event_id.is_empty() {
                last_event_id = Some(event_id);
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(poll_interval_secs)).await;
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
