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

use crate::error::Result;

pub mod c2w;
pub mod pi;
pub mod types;

pub use c2w::C2wHarness;
pub use pi::{HarnessImageResolver, PiHarness};
pub use types::{ContainerInfo, ContainerSpec, VolumeMount};

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
}
