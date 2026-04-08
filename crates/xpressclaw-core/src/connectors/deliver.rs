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
        other => {
            info!(
                connector_type = other,
                connector = connector_name,
                channel = channel_name,
                message,
                "sink delivery (stub connector — message logged only)"
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
