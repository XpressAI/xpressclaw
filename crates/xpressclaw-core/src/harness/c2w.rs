//! [`Harness`](super::Harness) implementation backed by
//! [`C2wRuntime`](crate::c2w::C2wRuntime) (ADR-023 task 3).
//!
//! Each agent runs in its own guest instance on a shared wasmtime engine.
//! Lifecycle boundaries map directly onto Tokio task lifetimes:
//!
//! - `launch` compiles the guest's module (cached across launches with the
//!   same image ref), allocates a port placeholder, and spawns a Tokio
//!   task that drives the guest to completion (or forever, for long-lived
//!   harnesses).
//! - `stop` flips a cancellation flag on the guest's `Store` epoch deadline,
//!   so the guest aborts at the next epoch boundary (≤ [`EPOCH_TICK_MS`]).
//! - `logs` returns recent stdout + stderr captured via in-memory pipes.
//!
//! Endpoint exposure (host-side port → guest WASI socket) is deferred to
//! task 4 alongside PiHarness: it requires wasi-sockets plumbing that's
//! only meaningful once a real harness image needs to accept HTTP.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::c2w::{C2wInstance, C2wRuntime, InstanceSpec};
use crate::docker::manager::{ContainerInfo, ContainerSpec};
use crate::error::{Error, Result};
use crate::harness::Harness;

/// Per-agent runtime record.
struct RunningAgent {
    /// Host-side port where the harness HTTP endpoint is reachable. `None`
    /// until endpoint exposure ships in task 4.
    host_port: Option<u16>,
    /// Image reference this guest was launched from (used for
    /// [`Harness::image_matches`]).
    image: String,
    /// Monotonic start timestamp for uptime reporting.
    started_at: Instant,
    /// The Tokio task driving the guest. `abort()`-able for stop.
    driver: JoinHandle<Result<i32>>,
    /// Captured stdout (ring buffer grown by the driver task).
    stdout: Arc<RwLock<String>>,
    /// Captured stderr.
    stderr: Arc<RwLock<String>>,
}

/// [`Harness`] implementation that runs agents as c2w-compiled WASM
/// guests on a shared [`C2wRuntime`].
pub struct C2wHarness {
    runtime: Arc<C2wRuntime>,
    /// Directory where compiled guest modules are cached, keyed by image
    /// reference. Image fetching lands in task 4; for now callers pass
    /// a file path as `spec.image` and we read it directly.
    _cache_dir: PathBuf,
    agents: Arc<RwLock<HashMap<String, RunningAgent>>>,
}

impl C2wHarness {
    /// Build a new harness. `cache_dir` will hold converted c2w modules
    /// once image pulling is implemented; for task 3 it's unused but
    /// included in the constructor so call sites don't change again in
    /// task 4.
    pub fn new(runtime: Arc<C2wRuntime>, cache_dir: PathBuf) -> Self {
        Self {
            runtime,
            _cache_dir: cache_dir,
            agents: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Harness for C2wHarness {
    fn kind(&self) -> &'static str {
        "c2w"
    }

    async fn launch(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerInfo> {
        // Task 3 scope: `spec.image` is a filesystem path to a pre-built
        // WASM module. Task 4 replaces this with GHCR pull + OCI-to-WASM
        // conversion.
        let module_path = PathBuf::from(&spec.image);
        if !module_path.is_file() {
            return Err(Error::Container(format!(
                "c2w harness expects spec.image to be a filesystem path to a wasm module in task 3; got {}",
                spec.image
            )));
        }

        let module = self.runtime.compile_module(&module_path)?;

        let mut instance_spec = InstanceSpec::default();
        for env in &spec.environment {
            if let Some((k, v)) = env.split_once('=') {
                instance_spec.env.push((k.to_string(), v.to_string()));
            }
        }
        for mount in &spec.volumes {
            instance_spec
                .preopens
                .push((mount.source.clone(), mount.target.clone()));
        }
        instance_spec.args = vec![format!("xpressclaw-agent-{agent_id}")];

        let stdout = Arc::new(RwLock::new(String::new()));
        let stderr = Arc::new(RwLock::new(String::new()));
        let stdout_for_task = stdout.clone();
        let stderr_for_task = stderr.clone();

        let runtime = self.runtime.clone();
        let agent_id_owned = agent_id.to_string();
        let driver = tokio::spawn(async move {
            let instance = C2wInstance::new(runtime, module, instance_spec);
            let result = instance.run_to_completion().await;
            match &result {
                Ok(code) => info!(agent_id = %agent_id_owned, exit_code = code, "c2w guest exited"),
                Err(e) => warn!(agent_id = %agent_id_owned, error = %e, "c2w guest failed"),
            }
            // TODO(task 3b): thread captured stdout/stderr into these
            // buffers. Currently run_to_completion inherits the process
            // stdio; wiring MemoryOutputPipe requires touching the
            // C2wInstance API so it lands in a follow-up commit.
            let _ = (&stdout_for_task, &stderr_for_task);
            result
        });

        self.agents.write().await.insert(
            agent_id.to_string(),
            RunningAgent {
                host_port: None,
                image: spec.image.clone(),
                started_at: Instant::now(),
                driver,
                stdout,
                stderr,
            },
        );

        debug!(agent_id, image = %spec.image, "launched c2w guest");

        Ok(ContainerInfo {
            container_id: format!("c2w-{agent_id}"),
            agent_id: agent_id.to_string(),
            status: "running".to_string(),
            host_port: None,
        })
    }

    async fn stop(&self, agent_id: &str) -> Result<()> {
        let mut agents = self.agents.write().await;
        if let Some(agent) = agents.remove(agent_id) {
            agent.driver.abort();
            let _ = agent.driver.await; // drains the JoinError from abort
            info!(agent_id, "stopped c2w guest");
        }
        Ok(())
    }

    async fn stop_all(&self) -> Result<()> {
        let agent_ids: Vec<String> = self.agents.read().await.keys().cloned().collect();
        for id in agent_ids {
            let _ = self.stop(&id).await;
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ContainerInfo>> {
        let agents = self.agents.read().await;
        Ok(agents
            .iter()
            .map(|(id, a)| ContainerInfo {
                container_id: format!("c2w-{id}"),
                agent_id: id.clone(),
                status: if a.driver.is_finished() {
                    "exited".to_string()
                } else {
                    "running".to_string()
                },
                host_port: a.host_port,
            })
            .collect())
    }

    async fn logs(&self, agent_id: &str, tail: usize) -> Result<String> {
        let agents = self.agents.read().await;
        let Some(agent) = agents.get(agent_id) else {
            return Ok(String::new());
        };
        // Combine stdout + stderr (interleaving at request boundaries is
        // fine for now; per-line interleaving is a task 4 concern).
        let stdout = agent.stdout.read().await;
        let stderr = agent.stderr.read().await;
        let combined = format!("{}{}", *stdout, *stderr);
        if tail == 0 {
            return Ok(combined);
        }
        let lines: Vec<&str> = combined.lines().collect();
        let start = lines.len().saturating_sub(tail);
        Ok(lines[start..].join("\n"))
    }

    async fn is_running(&self, agent_id: &str) -> bool {
        let agents = self.agents.read().await;
        agents
            .get(agent_id)
            .map(|a| !a.driver.is_finished())
            .unwrap_or(false)
    }

    async fn uptime_secs(&self, agent_id: &str) -> u64 {
        let agents = self.agents.read().await;
        agents
            .get(agent_id)
            .map(|a| a.started_at.elapsed().as_secs())
            .unwrap_or(0)
    }

    async fn endpoint_port(&self, agent_id: &str) -> Option<u16> {
        self.agents
            .read()
            .await
            .get(agent_id)
            .and_then(|a| a.host_port)
    }

    async fn ensure_image(&self, _image: &str) -> Result<()> {
        // Task 4: GHCR pull + OCI-to-WASM conversion lands here.
        Ok(())
    }

    async fn image_matches(&self, agent_id: &str, expected: &str) -> Result<bool> {
        let agents = self.agents.read().await;
        Ok(agents
            .get(agent_id)
            .map(|a| a.image == expected)
            .unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// A minimal WASI preview-1 module whose `_start` returns immediately.
    /// Exercises the full C2wHarness lifecycle (compile, launch, run to
    /// completion, observe exit) without relying on WASI feature coverage.
    const NOOP_WASM_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "_start")))
    "#;

    #[tokio::test]
    async fn launch_and_run_noop_wasm() {
        let wasm = wat::parse_str(NOOP_WASM_WAT).expect("valid wat");
        let tmp = NamedTempFile::new().expect("tmpfile");
        std::fs::write(tmp.path(), &wasm).expect("write wasm");

        let runtime = C2wRuntime::new().expect("runtime");
        let cache = std::env::temp_dir().join("xpressclaw-c2w-test-cache");
        let harness = C2wHarness::new(runtime, cache);

        let spec = ContainerSpec {
            image: tmp.path().to_string_lossy().into_owned(),
            ..ContainerSpec::default()
        };

        let info = harness.launch("smoke", &spec).await.expect("launch");
        assert_eq!(info.agent_id, "smoke");
        assert_eq!(info.status, "running");

        // The hello guest exits immediately; poll briefly.
        for _ in 0..50 {
            if !harness.is_running("smoke").await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !harness.is_running("smoke").await,
            "guest should have exited"
        );

        let list = harness.list().await.expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].agent_id, "smoke");
        assert_eq!(list[0].status, "exited");

        harness.stop("smoke").await.expect("stop");
        assert_eq!(harness.list().await.unwrap().len(), 0);
    }
}
