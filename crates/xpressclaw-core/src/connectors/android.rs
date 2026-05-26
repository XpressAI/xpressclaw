//! Android "device-link" connector.
//!
//! Surfaces a configured Android device (managed emulator or BYO) in the
//! Connectors UI: its config is the adb target, and `validate_config`/`health`
//! probe reachability via `adb_client` — the same shape as Telegram's `getMe`
//! check, except the "external dependency" is a device, not an API. It is
//! deliberately **not** an event source/sink: actual device control (tap,
//! screenshot) happens through the agent/MCP path. Placing it in the
//! connectors grid is a UX choice (see ADR-024). Feature-gated behind `android`.

use std::net::SocketAddr;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::info;

use crate::android::AndroidDevice;
use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// Which adb endpoint this connector links to.
#[derive(Clone)]
enum Target {
    /// Through the local adb server, by serial (e.g. `emulator-5554`).
    Server(String),
    /// Directly to a device's adbd over TCP (no adb server).
    Tcp(SocketAddr),
}

impl Target {
    fn label(&self) -> String {
        match self {
            Target::Server(s) => s.clone(),
            Target::Tcp(a) => a.to_string(),
        }
    }

    fn connect(&self) -> Result<AndroidDevice> {
        match self {
            Target::Server(serial) => AndroidDevice::via_server(serial),
            Target::Tcp(addr) => AndroidDevice::via_tcp(*addr),
        }
    }
}

/// Resolve the adb target from connector config. Prefers an explicit `tcp`
/// address, else a `serial`, defaulting to the standard emulator serial.
fn parse_target(config: &Value) -> Result<Target> {
    let tcp = config
        .get("tcp")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(tcp) = tcp {
        let addr: SocketAddr = tcp
            .parse()
            .map_err(|e| Error::Android(format!("invalid tcp address '{tcp}': {e}")))?;
        return Ok(Target::Tcp(addr));
    }
    let serial = config
        .get("serial")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("emulator-5554")
        .to_string();
    Ok(Target::Server(serial))
}

/// Probe device reachability. Blocking (adb_client is synchronous) — must be
/// called inside `spawn_blocking`.
fn probe(target: &Target) -> Result<()> {
    let mut device = target.connect()?;
    device.shell("echo xpressclaw")?;
    Ok(())
}

/// Connector that represents a link to one Android device.
pub struct AndroidConnector {
    target: Option<Target>,
}

impl Default for AndroidConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl AndroidConnector {
    pub fn new() -> Self {
        Self { target: None }
    }
}

#[async_trait]
impl Connector for AndroidConnector {
    fn connector_type(&self) -> &str {
        "android"
    }

    async fn validate_config(&self, config: &Value) -> ValidationResult {
        let target = match parse_target(config) {
            Ok(t) => t,
            Err(e) => {
                return ValidationResult {
                    valid: false,
                    error: Some(e.to_string()),
                }
            }
        };
        let label = target.label();
        match tokio::task::spawn_blocking(move || probe(&target)).await {
            Ok(Ok(())) => ValidationResult {
                valid: true,
                error: None,
            },
            Ok(Err(e)) => ValidationResult {
                valid: false,
                error: Some(format!("device '{label}' not reachable: {e}")),
            },
            Err(e) => ValidationResult {
                valid: false,
                error: Some(format!("device probe task failed: {e}")),
            },
        }
    }

    async fn start(
        &mut self,
        config: &Value,
        _channels: &[ChannelConfig],
        _event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        // No event source/sink — just record which device we're linked to.
        let target = parse_target(config)?;
        info!(target = %target.label(), "android device connector linked");
        self.target = Some(target);
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        self.target = None;
        Ok(())
    }

    async fn send(&self, _message: &SinkMessage) -> Result<()> {
        Err(Error::Android(
            "android connector is a device link, not a message sink — control happens \
             through the agent/MCP path"
                .to_string(),
        ))
    }

    async fn health(&self) -> bool {
        let Some(target) = self.target.clone() else {
            return false;
        };
        matches!(
            tokio::task::spawn_blocking(move || probe(&target)).await,
            Ok(Ok(()))
        )
    }
}
