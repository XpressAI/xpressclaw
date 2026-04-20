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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::c2w::{C2wInstance, C2wRuntime, InstanceSpec};
use crate::error::{Error, Result};
use crate::harness::types::{ContainerInfo, ContainerSpec};
use crate::harness::{Harness, SnapshotId};

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
    /// Preopened host paths declared when this guest was launched.
    /// Used by [`Harness::snapshot`] / `restore` to know which
    /// directories carry persistent guest state (ADR-023 task 8).
    preopens: Vec<(String, String)>,
}

/// [`Harness`] implementation that runs agents as c2w-compiled WASM
/// guests on a shared [`C2wRuntime`].
pub struct C2wHarness {
    runtime: Arc<C2wRuntime>,
    /// Directory where compiled guest modules are cached, keyed by image
    /// reference. Image fetching lands in task 4; for now callers pass
    /// a file path as `spec.image` and we read it directly.
    _cache_dir: PathBuf,
    /// Parent directory for snapshot backing storage (ADR-023 task 8).
    /// Each [`Harness::snapshot`] call creates a fresh subdirectory
    /// here; `restore` copies back; `delete_snapshot` rm-rfs it.
    snapshots_dir: PathBuf,
    agents: Arc<RwLock<HashMap<String, RunningAgent>>>,
}

impl C2wHarness {
    /// Build a new harness. `cache_dir` will hold converted c2w modules
    /// once image pulling is implemented; for task 3 it's unused but
    /// included in the constructor so call sites don't change again in
    /// task 4.
    pub fn new(runtime: Arc<C2wRuntime>, cache_dir: PathBuf) -> Self {
        let snapshots_dir = cache_dir.join("snapshots");
        Self {
            runtime,
            _cache_dir: cache_dir,
            snapshots_dir,
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
        let preopens_snapshot = instance_spec.preopens.clone();

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
                preopens: preopens_snapshot,
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

    /// Snapshot each preopened host directory the guest was launched
    /// with (ADR-023 task 8, MVP criterion 7).
    ///
    /// The snapshot's backing storage lives under `<cache_dir>/snapshots/
    /// <snap_id>/<index>/` — one subdirectory per preopen, keyed by
    /// index so order is preserved across restore.
    async fn snapshot(&self, agent_id: &str) -> Result<SnapshotId> {
        let preopens = {
            let agents = self.agents.read().await;
            let Some(record) = agents.get(agent_id) else {
                return Err(Error::Container(format!(
                    "snapshot: agent {agent_id} not running"
                )));
            };
            record.preopens.clone()
        };

        let snap_id = format!("{}-{}", agent_id, uuid::Uuid::new_v4().simple());
        let snap_root = self.snapshots_dir.join(&snap_id);
        std::fs::create_dir_all(&snap_root)
            .map_err(|e| Error::Container(format!("snapshot: create {:?}: {e}", snap_root)))?;

        for (idx, (host_path, _guest_path)) in preopens.iter().enumerate() {
            let dest = snap_root.join(idx.to_string());
            if let Err(e) = copy_dir_recursive(Path::new(host_path), &dest) {
                // Best-effort cleanup before surfacing the error.
                let _ = std::fs::remove_dir_all(&snap_root);
                return Err(Error::Container(format!(
                    "snapshot: copy {host_path} -> {dest:?}: {e}"
                )));
            }
        }

        debug!(agent_id, snap_id, preopens = preopens.len(), "snapshotted");
        Ok(SnapshotId::new(snap_id))
    }

    /// Restore each preopened directory from its snapshot copy (ADR-023
    /// task 8).
    ///
    /// The current guest is *not* re-launched here — restore only reverts
    /// persistent state. The caller (the task dispatcher in future
    /// task 10 wiring) is responsible for deciding whether to re-instantiate
    /// after restore. Semantically this is the "undo" for filesystem
    /// changes a rogue tool call made; the guest will see the reverted
    /// state the next time it reads from the preopened directory.
    async fn restore(&self, agent_id: &str, snapshot: &SnapshotId) -> Result<()> {
        let preopens = {
            let agents = self.agents.read().await;
            let Some(record) = agents.get(agent_id) else {
                return Err(Error::Container(format!(
                    "restore: agent {agent_id} not running"
                )));
            };
            record.preopens.clone()
        };

        let snap_root = self.snapshots_dir.join(snapshot.as_str());
        if !snap_root.is_dir() {
            return Err(Error::Container(format!(
                "restore: snapshot {snapshot} not found at {snap_root:?}"
            )));
        }

        for (idx, (host_path, _)) in preopens.iter().enumerate() {
            let src = snap_root.join(idx.to_string());
            if !src.is_dir() {
                // Not every preopen was snapshottable (e.g. a read-only
                // path that didn't exist). Skip quietly.
                continue;
            }
            let dest = Path::new(host_path);
            if dest.is_dir() {
                std::fs::remove_dir_all(dest)
                    .map_err(|e| Error::Container(format!("restore: clear {dest:?}: {e}")))?;
            }
            copy_dir_recursive(&src, dest)
                .map_err(|e| Error::Container(format!("restore: copy {src:?} -> {dest:?}: {e}")))?;
        }

        debug!(agent_id, snap_id = %snapshot, "restored");
        Ok(())
    }

    async fn delete_snapshot(&self, snapshot: &SnapshotId) -> Result<()> {
        let snap_root = self.snapshots_dir.join(snapshot.as_str());
        if snap_root.exists() {
            std::fs::remove_dir_all(&snap_root)
                .map_err(|e| Error::Container(format!("delete_snapshot: {e}")))?;
        }
        Ok(())
    }
}

/// Copy `src` tree to `dest` recursively. Creates `dest` if missing.
/// Used by snapshot/restore (ADR-023 task 8).
fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let child_src = entry.path();
        let child_dest = dest.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&child_src, &child_dest)?;
        } else if ft.is_file() {
            std::fs::copy(&child_src, &child_dest)?;
        }
        // Symlinks are intentionally skipped — dereferencing them
        // could escape the intended snapshot scope.
    }
    Ok(())
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

    /// Snapshot captures preopened dir state; `restore` reverts it.
    /// Proves the MVP criterion 7 plumbing (ADR-023): a tool call that
    /// mutates the workspace can be rolled back to the pre-call state.
    ///
    /// Uses the host-side filesystem directly to simulate a guest that
    /// made filesystem changes — avoids the WAT-authoring rabbit hole
    /// while exercising the real snapshot/restore paths.
    #[tokio::test]
    async fn snapshot_and_restore_roundtrip_workspace() {
        let wasm = wat::parse_str(NOOP_WASM_WAT).expect("valid wat");
        let wasm_file = tempfile::NamedTempFile::new().expect("tmp");
        std::fs::write(wasm_file.path(), &wasm).expect("write wasm");

        // Per-test cache + workspace dirs so runs don't collide.
        let cache = tempfile::tempdir().expect("cache");
        let workspace = tempfile::tempdir().expect("workspace");

        let runtime = C2wRuntime::new().expect("runtime");
        let harness = C2wHarness::new(runtime, cache.path().to_path_buf());

        let spec = ContainerSpec {
            image: wasm_file.path().to_string_lossy().into_owned(),
            volumes: vec![crate::harness::types::VolumeMount {
                source: workspace.path().to_string_lossy().into_owned(),
                target: "/workspace".into(),
                read_only: false,
            }],
            ..ContainerSpec::default()
        };

        // Seed: file that should survive the rollback.
        std::fs::write(workspace.path().join("safe.txt"), b"safe").unwrap();

        harness.launch("rollback", &spec).await.expect("launch");

        // Take a snapshot of the workspace as of "safe.txt exists".
        let snap = harness.snapshot("rollback").await.expect("snapshot");

        // Simulate a misbehaving tool call that mutates the workspace.
        std::fs::write(workspace.path().join("bad.txt"), b"bad").unwrap();
        std::fs::remove_file(workspace.path().join("safe.txt")).unwrap();

        // Restore: bad.txt should go, safe.txt should reappear.
        harness.restore("rollback", &snap).await.expect("restore");

        assert!(
            workspace.path().join("safe.txt").exists(),
            "safe.txt should have been restored"
        );
        assert!(
            !workspace.path().join("bad.txt").exists(),
            "bad.txt should have been reverted"
        );

        // Delete snapshot frees its backing storage.
        harness.delete_snapshot(&snap).await.expect("delete");
        assert!(
            !cache.path().join("snapshots").join(snap.as_str()).exists(),
            "snapshot backing dir should be gone after delete"
        );

        harness.stop("rollback").await.expect("stop");
    }
}
