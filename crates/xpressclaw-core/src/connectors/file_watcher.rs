use std::path::Path;

use async_trait::async_trait;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// File system watcher connector using the `notify` crate.
///
/// **Source**: Watches configured paths for file system events (create, modify, delete)
/// and emits `ConnectorEvent`s.
///
/// **Sink**: Writes rendered template content to a configured output path.
pub struct FileWatcherConnector {
    // The watcher must be kept alive for the duration of the watch.
    // Dropping it stops all watches.
    watcher: Option<RecommendedWatcher>,
    channels: Vec<ChannelConfig>,
    config: Value,
}

impl Default for FileWatcherConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWatcherConnector {
    pub fn new() -> Self {
        Self {
            watcher: None,
            channels: Vec::new(),
            config: Value::Null,
        }
    }
}

#[async_trait]
impl Connector for FileWatcherConnector {
    fn connector_type(&self) -> &str {
        "file_watcher"
    }

    async fn validate_config(&self, config: &Value) -> ValidationResult {
        let paths = config.get("paths").and_then(|v| v.as_array());

        match paths {
            Some(arr) if !arr.is_empty() => {
                // Check that paths are strings
                for p in arr {
                    if p.as_str().is_none() {
                        return ValidationResult {
                            valid: false,
                            error: Some("all paths must be strings".to_string()),
                        };
                    }
                }
                ValidationResult {
                    valid: true,
                    error: None,
                }
            }
            _ => ValidationResult {
                valid: false,
                error: Some("config must include a non-empty 'paths' array".to_string()),
            },
        }
    }

    async fn start(
        &mut self,
        config: &Value,
        channels: &[ChannelConfig],
        event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        self.channels = channels.to_vec();
        self.config = config.clone();

        let recursive = config
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let paths: Vec<String> = config
            .get("paths")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        if paths.is_empty() {
            return Err(Error::Connector(
                "no paths configured for file_watcher".to_string(),
            ));
        }

        let source_channels: Vec<ChannelConfig> = channels
            .iter()
            .filter(|ch| ch.channel_type == "source" || ch.channel_type == "both")
            .cloned()
            .collect();

        // Create the watcher with a callback that sends events
        let watcher =
            notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        let event_type = match event.kind {
                            EventKind::Create(_) => "created",
                            EventKind::Modify(_) => "modified",
                            EventKind::Remove(_) => "deleted",
                            _ => return, // Ignore other event kinds (access, other)
                        };

                        for path in &event.paths {
                            let path_str = path.to_string_lossy().to_string();
                            let payload = json!({
                                "path": path_str,
                                "event": event_type,
                            });

                            for channel in &source_channels {
                                let connector_event = ConnectorEvent {
                                    connector_id: channel.id.clone(),
                                    channel_id: channel.id.clone(),
                                    event_type: format!("file.{}", event_type),
                                    payload: payload.clone(),
                                };

                                // Use try_send since we're in a sync callback
                                if let Err(e) = event_tx.try_send(connector_event) {
                                    // Log but don't panic — the channel might be full or closed
                                    eprintln!("file_watcher: failed to send event: {}", e);
                                }
                            }

                            debug!(
                                path = path_str.as_str(),
                                event_type = event_type,
                                "file system event"
                            );
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "file watcher error");
                    }
                }
            })
            .map_err(|e| Error::Connector(format!("failed to create file watcher: {e}")))?;

        self.watcher = Some(watcher);

        // Watch all configured paths
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        for path_str in &paths {
            let path = Path::new(path_str);
            if !path.exists() {
                warn!(
                    path = path_str.as_str(),
                    "watch path does not exist, skipping"
                );
                continue;
            }

            if let Some(ref mut w) = self.watcher {
                w.watch(path, mode).map_err(|e| {
                    Error::Connector(format!("failed to watch {}: {}", path_str, e))
                })?;
                info!(
                    path = path_str.as_str(),
                    recursive = recursive,
                    "watching path"
                );
            }
        }

        info!(
            paths = paths.len(),
            channels = channels.len(),
            "file_watcher connector started"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("file_watcher connector stopping");
        // Dropping the watcher stops all watches
        self.watcher = None;
        self.channels.clear();
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        // Resolve output path from channel config or connector config
        let channel = self.channels.iter().find(|ch| ch.id == message.channel_id);

        let output_path = channel
            .and_then(|ch| ch.config.get("output_path").and_then(|v| v.as_str()))
            .or_else(|| self.config.get("output_path").and_then(|v| v.as_str()));

        let output_path = match output_path {
            Some(p) => p,
            None => {
                warn!(
                    channel_id = message.channel_id.as_str(),
                    "no output_path configured for file_watcher sink"
                );
                return Err(Error::Connector(
                    "no output_path configured for file_watcher sink channel".to_string(),
                ));
            }
        };

        let content = render_template(&message.template, &message.context);

        std::fs::write(output_path, &content)
            .map_err(|e| Error::Connector(format!("failed to write to {}: {}", output_path, e)))?;

        info!(
            path = output_path,
            channel_id = message.channel_id.as_str(),
            "file written"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        self.watcher.is_some()
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
