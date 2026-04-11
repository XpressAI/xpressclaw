//! Sink message delivery — sends messages through connectors.
//!
//! This module provides a standalone delivery function that can be called
//! from the workflow engine without needing a live ConnectorRegistry.
//! It looks up connector config from the DB and makes direct API calls.

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{error, info, warn};

use crate::db::Database;
use crate::error::{Error, Result};

use super::manager::ConnectorManager;

/// Deliver a rendered message through a connector's channel.
///
/// Looks up the connector and channel config from the DB, then sends
/// the message using the appropriate transport (HTTP for webhooks and
/// Telegram, filesystem for file_watcher, logs for stubs).
pub fn deliver(db: &Arc<Database>, connector_name: &str, channel_name: &str, message: &str) {
    let mgr = ConnectorManager::new(db.clone());

    // Find connector by name
    let connector = match find_connector_by_name(&mgr, connector_name) {
        Some(c) => c,
        None => {
            warn!(
                connector = connector_name,
                "sink delivery failed: connector not found"
            );
            return;
        }
    };

    // Find channel by name within this connector
    let channel = match find_channel_by_name(&mgr, &connector.id, channel_name) {
        Some(ch) => ch,
        None => {
            warn!(
                connector = connector_name,
                channel = channel_name,
                "sink delivery failed: channel not found"
            );
            return;
        }
    };

    // Dispatch based on connector type
    let result = match connector.connector_type.as_str() {
        "telegram" => deliver_telegram(&connector.config, &channel.config, message),
        "webhook" => deliver_webhook(&connector.config, &channel.config, message),
        "file_watcher" => deliver_file(&channel.config, message),
        "slack" => deliver_slack(&connector.config, &channel.config, message),
        "github" => deliver_github(&connector.config, &channel.config, message),
        "jira" => deliver_jira(&connector.config, &channel.config, message),
        "email" => deliver_email(&connector.config, &channel.config, message),
        other => {
            info!(
                connector_type = other,
                connector = connector_name,
                channel = channel_name,
                message,
                "sink delivery (unknown connector — message logged only)"
            );
            Ok(())
        }
    };

    match result {
        Ok(()) => {
            info!(
                connector = connector_name,
                channel = channel_name,
                "sink message delivered"
            );
        }
        Err(e) => {
            error!(
                connector = connector_name,
                channel = channel_name,
                error = %e,
                "sink delivery failed"
            );
        }
    }
}

fn find_connector_by_name(
    mgr: &ConnectorManager,
    name: &str,
) -> Option<super::manager::ConnectorRecord> {
    mgr.list().ok()?.into_iter().find(|c| c.name == name)
}

fn find_channel_by_name(
    mgr: &ConnectorManager,
    connector_id: &str,
    name: &str,
) -> Option<super::manager::ChannelRecord> {
    mgr.list_channels(connector_id)
        .ok()?
        .into_iter()
        .find(|ch| ch.name == name)
}

/// Send via Telegram Bot API sendMessage.
fn deliver_telegram(connector_config: &Value, channel_config: &Value, message: &str) -> Result<()> {
    let token = connector_config
        .get("bot_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("telegram: missing bot_token".into()))?
        .to_string();

    let chat_id = channel_config
        .get("chat_id")
        .cloned()
        .ok_or_else(|| Error::Connector("telegram: missing chat_id in channel config".into()))?;

    let message = message.to_string();
    tokio::spawn(async move {
        let url = format!("https://api.telegram.org/bot{token}/sendMessage");
        let body = json!({
            "chat_id": chat_id,
            "text": message,
            "parse_mode": "Markdown"
        });
        let client = reqwest::Client::new();
        if let Err(e) = client.post(&url).json(&body).send().await {
            error!(error = %e, "telegram sendMessage failed");
        }
    });

    Ok(())
}

/// Send via HTTP POST to webhook URL.
fn deliver_webhook(connector_config: &Value, channel_config: &Value, message: &str) -> Result<()> {
    let url = channel_config
        .get("url")
        .and_then(|v| v.as_str())
        .or_else(|| connector_config.get("url").and_then(|v| v.as_str()))
        .ok_or_else(|| Error::Connector("webhook: no URL configured".into()))?
        .to_string();

    let body = json!({ "text": message });
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        if let Err(e) = client.post(&url).json(&body).send().await {
            error!(error = %e, "webhook delivery failed");
        }
    });

    Ok(())
}

/// Write message to a file.
fn deliver_file(channel_config: &Value, message: &str) -> Result<()> {
    let path = channel_config
        .get("output_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("file_watcher: no output_path in channel config".into()))?;

    std::fs::write(path, message)
        .map_err(|e| Error::Connector(format!("file write failed: {e}")))?;

    Ok(())
}

/// Send via Slack chat.postMessage API.
fn deliver_slack(connector_config: &Value, channel_config: &Value, message: &str) -> Result<()> {
    let bot_token = connector_config
        .get("bot_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("slack: missing bot_token".into()))?
        .to_string();

    let channel_id = channel_config
        .get("channel_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("slack: missing channel_id in channel config".into()))?
        .to_string();

    let message = message.to_string();
    tokio::spawn(async move {
        let body = json!({
            "channel": channel_id,
            "text": message,
        });
        let client = reqwest::Client::new();
        if let Err(e) = client
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {}", bot_token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()
            .await
        {
            error!(error = %e, "slack chat.postMessage failed");
        }
    });

    Ok(())
}

/// Send via GitHub API (comment on issue or create issue).
fn deliver_github(connector_config: &Value, _channel_config: &Value, message: &str) -> Result<()> {
    let token = connector_config
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("github: missing token".into()))?
        .to_string();

    let owner = connector_config
        .get("owner")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("github: missing owner".into()))?
        .to_string();

    let repo = connector_config
        .get("repo")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("github: missing repo".into()))?
        .to_string();

    // Try to parse message as JSON to extract issue_number for comments;
    // otherwise create a new issue with the message as body.
    let message = message.to_string();
    tokio::spawn(async move {
        let client = reqwest::Client::builder()
            .user_agent("xpressclaw/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        // Attempt to parse JSON to check for issue_number
        let parsed: Option<Value> = serde_json::from_str(&message).ok();
        let issue_number = parsed
            .as_ref()
            .and_then(|v| v.get("issue_number"))
            .and_then(|v| v.as_u64());

        if let Some(num) = issue_number {
            let url = format!(
                "https://api.github.com/repos/{}/{}/issues/{}/comments",
                owner, repo, num
            );
            let body_text = parsed
                .as_ref()
                .and_then(|v| v.get("body"))
                .and_then(|v| v.as_str())
                .unwrap_or(&message);
            let body = json!({ "body": body_text });
            if let Err(e) = client
                .post(&url)
                .header("Authorization", format!("token {}", token))
                .header("Accept", "application/vnd.github.v3+json")
                .json(&body)
                .send()
                .await
            {
                error!(error = %e, "github comment POST failed");
            }
        } else {
            let url = format!("https://api.github.com/repos/{}/{}/issues", owner, repo);
            let body = json!({
                "title": "New issue from xpressclaw",
                "body": message,
            });
            if let Err(e) = client
                .post(&url)
                .header("Authorization", format!("token {}", token))
                .header("Accept", "application/vnd.github.v3+json")
                .json(&body)
                .send()
                .await
            {
                error!(error = %e, "github create issue failed");
            }
        }
    });

    Ok(())
}

/// Send via Jira API (add comment to issue).
fn deliver_jira(connector_config: &Value, _channel_config: &Value, message: &str) -> Result<()> {
    use base64::Engine as _;

    let base_url = connector_config
        .get("base_url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("jira: missing base_url".into()))?
        .trim_end_matches('/')
        .to_string();

    let email = connector_config
        .get("email")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("jira: missing email".into()))?
        .to_string();

    let api_token = connector_config
        .get("api_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("jira: missing api_token".into()))?
        .to_string();

    // Try to extract issue key from JSON message
    let parsed: Option<Value> = serde_json::from_str(message).ok();
    let issue_key = parsed
        .as_ref()
        .and_then(|v| v.get("issue_key").or(v.get("key")))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            Error::Connector("jira: message must contain issue_key or key field".into())
        })?
        .to_string();

    let body_text = parsed
        .as_ref()
        .and_then(|v| v.get("body"))
        .and_then(|v| v.as_str())
        .unwrap_or(message)
        .to_string();

    tokio::spawn(async move {
        let credentials = format!("{}:{}", email, api_token);
        let encoded = base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes());
        let auth = format!("Basic {}", encoded);

        let url = format!("{}/rest/api/3/issue/{}/comment", base_url, issue_key);
        let body = json!({
            "body": {
                "type": "doc",
                "version": 1,
                "content": [{
                    "type": "paragraph",
                    "content": [{
                        "type": "text",
                        "text": body_text
                    }]
                }]
            }
        });

        let client = reqwest::Client::new();
        if let Err(e) = client
            .post(&url)
            .header("Authorization", &auth)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            error!(error = %e, "jira comment POST failed");
        }
    });

    Ok(())
}

/// Send via SMTP email.
fn deliver_email(connector_config: &Value, channel_config: &Value, message: &str) -> Result<()> {
    let smtp_host = connector_config
        .get("smtp_host")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("email: missing smtp_host".into()))?
        .to_string();

    let smtp_port = connector_config
        .get("smtp_port")
        .and_then(|v| v.as_u64())
        .unwrap_or(587) as u16;

    let username = connector_config
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("email: missing username".into()))?
        .to_string();

    let password = connector_config
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("email: missing password".into()))?
        .to_string();

    let to_addr = channel_config
        .get("to")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Connector("email: missing 'to' in channel config".into()))?
        .to_string();

    let message = message.to_string();
    let from = username.clone();

    tokio::spawn(async move {
        use lettre::message::header::ContentType;
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

        let email = match Message::builder()
            .from(match from.parse() {
                Ok(addr) => addr,
                Err(e) => {
                    error!(error = %e, "invalid from address for email delivery");
                    return;
                }
            })
            .to(match to_addr.parse() {
                Ok(addr) => addr,
                Err(e) => {
                    error!(error = %e, "invalid to address for email delivery");
                    return;
                }
            })
            .subject("Message from xpressclaw")
            .header(ContentType::TEXT_PLAIN)
            .body(message)
        {
            Ok(e) => e,
            Err(e) => {
                error!(error = %e, "failed to build email");
                return;
            }
        };

        let creds = Credentials::new(username, password);

        let mailer = match AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp_host) {
            Ok(builder) => builder.port(smtp_port).credentials(creds).build(),
            Err(e) => {
                error!(error = %e, "failed to create SMTP transport");
                return;
            }
        };

        if let Err(e) = mailer.send(email).await {
            error!(error = %e, "SMTP send failed");
        }
    });

    Ok(())
}
