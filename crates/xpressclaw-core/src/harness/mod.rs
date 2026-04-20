//! Harness abstraction (ADR-023).
//!
//! A [`Harness`] runs and manages agent workloads. Today the only
//! implementation is a thin wrapper around [`DockerManager`]; ADR-023
//! schedules replacing it with a container2wasm + wasmtime implementation
//! and deleting the Docker code entirely.
//!
//! The trait is deliberately narrow — it covers the lifecycle + endpoint +
//! observability surface the reconciler, task dispatcher, and message
//! processor actually consume, and nothing more. Image management,
//! snapshotting, and tmux-attach are follow-ups landing in subsequent
//! tasks of the ADR-023 implementation.

use async_trait::async_trait;

use crate::docker::manager::{ContainerInfo, ContainerSpec, DockerManager};
use crate::error::Result;

pub use crate::docker::manager::{ContainerInfo as HarnessInfo, VolumeMount};

pub mod c2w;
pub mod pi;
pub use c2w::C2wHarness;
pub use pi::{HarnessImageResolver, PiHarness};

/// Runtime abstraction over agent workloads.
///
/// Implementors own the lifecycle (launch/stop/observe) of agents. The
/// trait identifies agents by their `agent_id` from config; implementations
/// map internally to whatever container / instance naming they use.
#[async_trait]
pub trait Harness: Send + Sync {
    /// Short identifier for this harness implementation (e.g. `"docker"`,
    /// `"c2w"`). Used in logs and for config routing.
    fn kind(&self) -> &'static str;

    /// Launch an agent workload. On success the returned `ContainerInfo`
    /// carries the host-side port where the in-workload HTTP endpoint
    /// (OpenAI-compatible / session API) is reachable.
    async fn launch(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerInfo>;

    /// Stop a single agent. Idempotent — returns Ok even if the agent is
    /// already stopped.
    async fn stop(&self, agent_id: &str) -> Result<()>;

    /// Stop every agent this harness currently manages. Called on server
    /// shutdown.
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
    /// reachable, or `None` if the agent isn't running / hasn't bound a
    /// port yet.
    async fn endpoint_port(&self, agent_id: &str) -> Option<u16>;

    /// Ensure the workload image referenced by `spec.image` is available
    /// locally. Implementations that don't fetch images (in-process,
    /// pre-bundled) should make this a no-op.
    async fn ensure_image(&self, image: &str) -> Result<()>;

    /// Is the image for this agent's running instance the same as
    /// `expected`? Used by the reconciler to detect drift when the agent
    /// config's image changes.
    async fn image_matches(&self, agent_id: &str, expected: &str) -> Result<bool>;
}

/// [`Harness`] implementation backed by Docker / Podman via `bollard`.
///
/// This is scaffolding for ADR-023 — it wraps the existing [`DockerManager`]
/// so the rest of the codebase can migrate to `Arc<dyn Harness>` in
/// small, reversible steps. When the c2w-backed `C2wHarness` lands and the
/// migration completes, the Docker module (and this impl along with it)
/// is deleted per ADR-023 decision 4.
#[async_trait]
impl Harness for DockerManager {
    fn kind(&self) -> &'static str {
        "docker"
    }

    async fn launch(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerInfo> {
        DockerManager::launch(self, agent_id, spec).await
    }

    async fn stop(&self, agent_id: &str) -> Result<()> {
        DockerManager::stop(self, agent_id).await
    }

    async fn stop_all(&self) -> Result<()> {
        DockerManager::stop_all(self).await
    }

    async fn list(&self) -> Result<Vec<ContainerInfo>> {
        DockerManager::list(self).await
    }

    async fn logs(&self, agent_id: &str, tail: usize) -> Result<String> {
        DockerManager::logs(self, agent_id, tail).await
    }

    async fn is_running(&self, agent_id: &str) -> bool {
        DockerManager::is_container_running(self, &format!("xpressclaw-{agent_id}")).await
    }

    async fn uptime_secs(&self, agent_id: &str) -> u64 {
        DockerManager::container_uptime_secs(self, &format!("xpressclaw-{agent_id}")).await
    }

    async fn endpoint_port(&self, agent_id: &str) -> Option<u16> {
        let container_id =
            DockerManager::get_container_id(self, &format!("xpressclaw-{agent_id}")).await?;
        DockerManager::get_container_port(self, &container_id).await
    }

    async fn ensure_image(&self, image: &str) -> Result<()> {
        if DockerManager::has_image(self, image).await {
            return Ok(());
        }
        DockerManager::pull_image(self, image).await
    }

    async fn image_matches(&self, agent_id: &str, expected: &str) -> Result<bool> {
        Ok(DockerManager::container_image_matches(
            self,
            &format!("xpressclaw-{agent_id}"),
            expected,
        )
        .await)
    }
}
