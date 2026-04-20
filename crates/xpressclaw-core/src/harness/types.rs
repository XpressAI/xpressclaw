//! Shared types for the [`Harness`](super::Harness) trait.
//!
//! These used to live in the Docker module alongside `DockerManager`.
//! ADR-023 removed Docker, and the types outlived the removal because
//! they describe the generic "launch spec / running info" contract all
//! harness implementations honor. Each field is a hint; some harnesses
//! ignore some fields (c2w, for instance, doesn't use `network_mode`).

use serde::{Deserialize, Serialize};

/// Specification for launching an agent workload.
///
/// Harnesses interpret each field according to their own semantics:
/// - `image` — for `PiHarness`, a filesystem path or OCI ref that
///   resolves to a WASM module; other harnesses may interpret it
///   differently.
/// - `memory_limit` / `cpu_limit` — advisory; enforced where the
///   runtime supports it.
/// - `environment` — `KEY=VALUE` strings passed to the guest via WASI.
/// - `volumes` — host → guest mount mappings; implementations may
///   restrict the set of valid mappings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub image: String,
    pub memory_limit: Option<i64>,
    pub cpu_limit: Option<i64>,
    #[serde(default)]
    pub environment: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    pub network_mode: Option<String>,
    /// Port to expose from the workload (harness HTTP endpoint).
    pub expose_port: Option<u16>,
    /// Command / entrypoint override.
    pub cmd: Option<Vec<String>>,
    /// Working directory inside the workload.
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

impl Default for ContainerSpec {
    fn default() -> Self {
        Self {
            image: String::new(),
            memory_limit: Some(2 * 1024 * 1024 * 1024),
            cpu_limit: None,
            environment: Vec::new(),
            volumes: Vec::new(),
            network_mode: None,
            expose_port: None,
            cmd: None,
            working_dir: None,
        }
    }
}

/// Info about a launched agent workload returned by
/// [`Harness::launch`](super::Harness::launch) and
/// [`Harness::list`](super::Harness::list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub container_id: String,
    pub agent_id: String,
    pub status: String,
    pub host_port: Option<u16>,
}
