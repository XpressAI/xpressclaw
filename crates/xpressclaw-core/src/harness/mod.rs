//! Harness abstraction (ADR-023).
//!
//! A [`Harness`] runs and manages agent workloads. Docker has been
//! removed per ADR-023 decision 4; the only implementations today are
//! [`C2wHarness`] (generic c2w + wasmtime) and [`PiHarness`] (pi-agent
//! on top of c2w). More harnesses (codex, opencode, ...) live behind
//! this trait in follow-up ADRs.
//!
//! The trait is deliberately narrow — it covers the lifecycle +
//! endpoint + observability surface the reconciler, task dispatcher,
//! and message processor actually consume, and nothing more.

use async_trait::async_trait;

use crate::error::{Error, Result};

pub mod c2w;
pub mod pi;
pub mod types;

pub use c2w::C2wHarness;
pub use pi::{HarnessImageResolver, PiHarness};
pub use types::{ContainerInfo, ContainerSpec, VolumeMount};

/// Opaque handle to a saved point-in-time copy of an agent's guest
/// state (ADR-023 task 8).
///
/// Returned by [`Harness::snapshot`]; passed to
/// [`Harness::restore`] or [`Harness::delete_snapshot`]. The contents
/// are implementation-specific — callers treat these as opaque tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotId(pub String);

impl SnapshotId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Runtime abstraction over agent workloads.
///
/// Implementors own the lifecycle (launch/stop/observe) of agents. The
/// trait identifies agents by their `agent_id` from config;
/// implementations map internally to whatever naming they use.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Short identifier for this harness implementation (e.g. `"c2w"`,
    /// `"pi"`). Used in logs and for config routing.
    fn kind(&self) -> &'static str;

    /// Launch an agent workload. On success the returned `ContainerInfo`
    /// carries the host-side port where the in-workload HTTP endpoint
    /// is reachable (for harnesses that expose one).
    async fn launch(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerInfo>;

    /// Stop a single agent. Idempotent — returns Ok even if the agent
    /// is already stopped.
    async fn stop(&self, agent_id: &str) -> Result<()>;

    /// Stop every agent this harness currently manages. Called on
    /// server shutdown.
    async fn stop_all(&self) -> Result<()>;

    /// List info for every agent this harness is currently running.
    async fn list(&self) -> Result<Vec<ContainerInfo>>;

    /// Fetch recent stdout/stderr output for an agent. `tail` is the
    /// maximum number of lines to return.
    async fn logs(&self, agent_id: &str, tail: usize) -> Result<String>;

    /// Liveness check — is the agent currently running?
    async fn is_running(&self, agent_id: &str) -> bool;

    /// Seconds since the agent last started, or 0 if not running.
    async fn uptime_secs(&self, agent_id: &str) -> u64;

    /// Host-side port where the agent's harness HTTP endpoint is
    /// reachable, or `None` if the agent isn't running / hasn't bound
    /// a port yet.
    async fn endpoint_port(&self, agent_id: &str) -> Option<u16>;

    /// Ensure the workload image referenced by `spec.image` is
    /// available locally. Implementations that don't fetch images
    /// should make this a no-op.
    async fn ensure_image(&self, image: &str) -> Result<()>;

    /// Is the image for this agent's running instance the same as
    /// `expected`? Used by the reconciler to detect drift when the
    /// agent config's image changes.
    async fn image_matches(&self, agent_id: &str, expected: &str) -> Result<bool>;

    /// Capture the agent's current guest state so it can be restored
    /// later (ADR-023 task 8, MVP criterion 7).
    ///
    /// The default implementation returns an "unsupported" error. c2w
    /// and pi harnesses override it to copy preopen directories.
    /// Snapshots are used as pre-step checkpoints by the task
    /// dispatcher — when a tool call fails or the guest traps, the
    /// dispatcher restores to the prior snapshot and re-instantiates
    /// the guest.
    async fn snapshot(&self, agent_id: &str) -> Result<SnapshotId> {
        Err(Error::Container(format!(
            "snapshot not supported by {} harness (agent {agent_id})",
            self.kind()
        )))
    }

    /// Restore `agent_id`'s guest state to a prior snapshot (ADR-023
    /// task 8). Implementations stop the current guest, revert
    /// persistent state, and re-launch from the restored state.
    async fn restore(&self, agent_id: &str, snapshot: &SnapshotId) -> Result<()> {
        let _ = snapshot;
        Err(Error::Container(format!(
            "restore not supported by {} harness (agent {agent_id})",
            self.kind()
        )))
    }

    /// Free a snapshot's backing state. Callers use this after a
    /// successful tool call whose checkpoint is no longer needed.
    /// Implementations that don't track snapshots treat this as a
    /// no-op (return Ok).
    async fn delete_snapshot(&self, snapshot: &SnapshotId) -> Result<()> {
        let _ = snapshot;
        Ok(())
    }

    /// Return a host-side descriptor the frontend can use to attach
    /// to this agent's tmux session (ADR-023 task 9). Harnesses that
    /// don't expose tmux (e.g. a pure c2w WASM guest) return `None`.
    ///
    /// Concrete descriptor shape is intentionally opaque at this layer
    /// — callers resolve it via [`Harness::kind`]. The typical tmux
    /// harness returns the unix-socket path of the tmux server so a
    /// host-side xterm.js bridge can attach.
    async fn attach_tmux(&self, agent_id: &str) -> Option<TmuxAttach> {
        let _ = agent_id;
        None
    }
}

/// Descriptor for attaching to a harness's tmux session (ADR-023 task 9).
///
/// Not all harnesses expose tmux; those that do return this from
/// [`Harness::attach_tmux`]. The frontend uses `session_name` + the
/// harness-local `socket_path` to tunnel a terminal view to the user.
#[derive(Debug, Clone)]
pub struct TmuxAttach {
    pub session_name: String,
    pub socket_path: std::path::PathBuf,
}
