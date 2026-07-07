use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

use crate::db::Database;
use crate::error::{Error, Result};

use super::email::EmailConnector;
use super::file_watcher::FileWatcherConnector;
use super::github::GitHubConnector;
use super::jira::JiraConnector;
use super::manager::ConnectorManager;
use super::slack::SlackConnector;
use super::telegram::TelegramConnector;
use super::traits::{ChannelConfig, Connector, ConnectorEvent};
use super::webhook::WebhookConnector;

/// Manages live connector instances and their lifecycle.
///
/// The registry owns the running connector trait objects and coordinates
/// starting/stopping them based on database state.
pub struct ConnectorRegistry {
    db: Arc<Database>,
    connectors: RwLock<HashMap<String, Box<dyn Connector>>>,
    event_tx: mpsc::Sender<ConnectorEvent>,
    event_rx: Option<mpsc::Receiver<ConnectorEvent>>,
}

impl ConnectorRegistry {
    /// Create a new registry backed by the given database.
    ///
    /// An internal mpsc channel (capacity 1000) is created for connector events.
    /// Use [`take_event_receiver`] to obtain the receiving end before starting
    /// connectors.
    pub fn new(db: Arc<Database>) -> Self {
        let (event_tx, event_rx) = mpsc::channel(1000);
        Self {
            db,
            connectors: RwLock::new(HashMap::new()),
            event_tx,
            event_rx: Some(event_rx),
        }
    }

    /// Take the event receiver for the event processing loop.
    ///
    /// This can only be called once; subsequent calls return `None`.
    pub fn take_event_receiver(&mut self) -> Option<mpsc::Receiver<ConnectorEvent>> {
        self.event_rx.take()
    }

    /// Start all enabled connectors from the database.
    pub async fn start_all(&self) -> Result<()> {
        let mgr = ConnectorManager::new(self.db.clone());
        let records = mgr.list()?;

        for record in records {
            if !record.enabled {
                continue;
            }

            if let Err(e) = self.start_connector_inner(&mgr, &record.id).await {
                error!(
                    connector_id = record.id.as_str(),
                    name = record.name.as_str(),
                    error = %e,
                    "failed to start connector"
                );
                let _ = mgr.set_status(&record.id, "error", Some(&e.to_string()));
            }
        }

        Ok(())
    }

    /// Stop all running connectors.
    pub async fn stop_all(&self) -> Result<()> {
        let mgr = ConnectorManager::new(self.db.clone());
        let mut connectors = self.connectors.write().await;

        for (id, connector) in connectors.iter_mut() {
            info!(connector_id = id.as_str(), "stopping connector");
            if let Err(e) = connector.stop().await {
                error!(
                    connector_id = id.as_str(),
                    error = %e,
                    "error stopping connector"
                );
            }
            let _ = mgr.set_status(id, "stopped", None);
        }

        connectors.clear();
        Ok(())
    }

    /// Start a single connector by ID.
    pub async fn start_connector(&self, id: &str) -> Result<()> {
        let mgr = ConnectorManager::new(self.db.clone());
        self.start_connector_inner(&mgr, id).await
    }

    /// Stop a single connector by ID.
    pub async fn stop_connector(&self, id: &str) -> Result<()> {
        let mgr = ConnectorManager::new(self.db.clone());
        let mut connectors = self.connectors.write().await;

        match connectors.get_mut(id) {
            Some(connector) => {
                info!(connector_id = id, "stopping connector");
                connector.stop().await?;
                connectors.remove(id);
                let _ = mgr.set_status(id, "stopped", None);
                Ok(())
            }
            None => {
                warn!(connector_id = id, "connector not running, nothing to stop");
                Ok(())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    async fn start_connector_inner(&self, mgr: &ConnectorManager, id: &str) -> Result<()> {
        let record = mgr.get(id)?;
        let channel_records = mgr.list_channels(id)?;

        let channels: Vec<ChannelConfig> = channel_records
            .into_iter()
            .filter(|ch| ch.enabled)
            .map(|ch| ChannelConfig {
                id: ch.id,
                name: ch.name,
                channel_type: ch.channel_type,
                config: ch.config,
                agent_id: ch.agent_id,
            })
            .collect();

        let mut connector = create_connector(&record.connector_type);

        // Validate config first
        let validation = connector.validate_config(&record.config).await;
        if !validation.valid {
            let err_msg = validation
                .error
                .unwrap_or_else(|| "invalid configuration".to_string());
            mgr.set_status(id, "error", Some(&err_msg))?;
            return Err(Error::Connector(err_msg));
        }

        info!(
            connector_id = id,
            connector_type = record.connector_type.as_str(),
            channels = channels.len(),
            "starting connector"
        );

        connector
            .start(&record.config, &channels, self.event_tx.clone())
            .await?;

        mgr.set_status(id, "running", None)?;

        let mut connectors = self.connectors.write().await;
        connectors.insert(id.to_string(), connector);

        Ok(())
    }
}

/// Create a connector instance by type name.
fn create_connector(connector_type: &str) -> Box<dyn Connector> {
    match connector_type {
        "webhook" => Box::new(WebhookConnector::new()),
        "telegram" => Box::new(TelegramConnector::new()),
        "file_watcher" => Box::new(FileWatcherConnector::new()),
        "email" => Box::new(EmailConnector::new()),
        "github" => Box::new(GitHubConnector::new()),
        "jira" => Box::new(JiraConnector::new()),
        "slack" => Box::new(SlackConnector::new()),
        _ => {
            warn!(
                connector_type = connector_type,
                "unknown connector type, using webhook as fallback"
            );
            Box::new(WebhookConnector::new())
        }
    }
}
