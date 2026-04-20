//! `xpressclaw rollback-smoke` — end-to-end smoke for snapshot/restore
//! (ADR-023 task 8, MVP criterion 7).
//!
//! Demonstrates the rollback-on-failure primitive without needing a
//! real agent. Launches a c2w guest, seeds a "safe" file in the
//! workspace, snapshots, simulates a misbehaving tool call that
//! rewrites the workspace, and then restores the snapshot to verify
//! the filesystem comes back.
//!
//! Removed once a real pi-agent flow (tasks 5/6/10) subsumes it as a
//! more realistic end-to-end smoke.

use std::sync::Arc;

use tempfile::{tempdir, NamedTempFile};
use xpressclaw_core::c2w::C2wRuntime;
use xpressclaw_core::harness::types::{ContainerSpec, VolumeMount};
use xpressclaw_core::harness::{C2wHarness, Harness};

const NOOP_WASM_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "_start")))
"#;

pub async fn run() -> anyhow::Result<()> {
    println!("==> Building wasmtime engine...");
    let runtime = C2wRuntime::new()?;

    println!("==> Compiling noop WASI guest...");
    let wasm = wat::parse_str(NOOP_WASM_WAT)?;
    let wasm_file = NamedTempFile::new()?;
    std::fs::write(wasm_file.path(), &wasm)?;

    let cache = tempdir()?;
    let workspace = tempdir()?;
    println!("    cache_dir: {}", cache.path().display());
    println!("    workspace: {}", workspace.path().display());

    let harness: Arc<dyn Harness> = Arc::new(C2wHarness::new(runtime, cache.path().to_path_buf()));

    let spec = ContainerSpec {
        image: wasm_file.path().to_string_lossy().into_owned(),
        volumes: vec![VolumeMount {
            source: workspace.path().to_string_lossy().into_owned(),
            target: "/workspace".into(),
            read_only: false,
        }],
        ..ContainerSpec::default()
    };

    // Seed a file whose existence represents "important state the
    // rogue tool call shouldn't be able to destroy."
    let safe_path = workspace.path().join("important.txt");
    std::fs::write(&safe_path, b"important state\n")?;
    println!("==> Seeded {}", safe_path.display());

    println!("==> Launching guest as agent 'rollback'...");
    harness.launch("rollback", &spec).await?;

    println!("==> Snapshotting workspace as pre-tool-call checkpoint...");
    let snap = harness.snapshot("rollback").await?;
    println!("    snapshot: {snap}");

    println!("==> Simulating rogue tool call: deleting important.txt, writing garbage.txt...");
    std::fs::remove_file(&safe_path)?;
    std::fs::write(workspace.path().join("garbage.txt"), b"malicious\n")?;
    println!(
        "    pre-restore: important.txt exists = {}, garbage.txt exists = {}",
        safe_path.exists(),
        workspace.path().join("garbage.txt").exists()
    );

    println!("==> Restoring snapshot...");
    harness.restore("rollback", &snap).await?;
    println!(
        "    post-restore: important.txt exists = {}, garbage.txt exists = {}",
        safe_path.exists(),
        workspace.path().join("garbage.txt").exists()
    );

    if !safe_path.exists() {
        anyhow::bail!("important.txt did not come back after restore");
    }
    if workspace.path().join("garbage.txt").exists() {
        anyhow::bail!("garbage.txt was not removed by restore");
    }

    println!("==> Deleting snapshot backing storage...");
    harness.delete_snapshot(&snap).await?;

    harness.stop("rollback").await?;
    println!("==> Smoke test passed.");
    Ok(())
}
