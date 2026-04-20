//! `xpressclaw pi-smoke` — end-to-end smoke test for the pi harness.
//!
//! Builds the same noop WASM guest the c2w-smoke uses, then launches it
//! through a PiHarness so the pi-specific defaults (workspace mount,
//! LLM/xclaw env seeding) are exercised. Temporary — replaced by the
//! real pi launch flow once tasks 5/6 wire up the xclaw bridge and LLM
//! sidecar.

use std::sync::Arc;
use std::time::Duration;

use tempfile::{tempdir, NamedTempFile};
use xpressclaw_core::c2w::C2wRuntime;
use xpressclaw_core::docker::manager::ContainerSpec;
use xpressclaw_core::harness::{C2wHarness, Harness, HarnessImageResolver, PiHarness};

const NOOP_WASM_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "_start")))
"#;

pub async fn run() -> anyhow::Result<()> {
    println!("==> Building wasmtime engine + epoch driver...");
    let runtime = C2wRuntime::new()?;

    println!("==> Compiling noop WASI guest...");
    let wasm = wat::parse_str(NOOP_WASM_WAT)?;
    let wasm_file = NamedTempFile::new()?;
    std::fs::write(wasm_file.path(), &wasm)?;

    let cache = tempdir()?;
    let workspaces = tempdir()?;
    println!("    cache_dir:       {}", cache.path().display());
    println!("    workspaces_root: {}", workspaces.path().display());

    let c2w = Arc::new(C2wHarness::new(runtime, cache.path().to_path_buf()));
    let pi = PiHarness::new(
        c2w,
        HarnessImageResolver::new(cache.path().to_path_buf()),
        workspaces.path().to_path_buf(),
    );
    let pi: Arc<dyn Harness> = Arc::new(pi);

    let spec = ContainerSpec {
        image: wasm_file.path().to_string_lossy().into_owned(),
        ..ContainerSpec::default()
    };

    println!("==> Launching guest as agent 'piggy' through PiHarness...");
    let info = pi.launch("piggy", &spec).await?;
    println!("    container_id: {}", info.container_id);
    println!("    harness kind: {}", pi.kind());

    // Verify the pi-specific side effects:
    let workspace_dir = workspaces.path().join("piggy");
    if !workspace_dir.is_dir() {
        anyhow::bail!(
            "expected workspace {} to exist, but it doesn't",
            workspace_dir.display()
        );
    }
    println!("    workspace created: {}", workspace_dir.display());

    println!("==> Waiting for guest to exit...");
    for _ in 0..100 {
        if !pi.is_running("piggy").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    if pi.is_running("piggy").await {
        anyhow::bail!("guest still running after 2s");
    }

    let list = pi.list().await?;
    let entry = list
        .iter()
        .find(|i| i.agent_id == "piggy")
        .ok_or_else(|| anyhow::anyhow!("piggy missing from list"))?;
    println!("    final status: {}", entry.status);
    println!("    uptime:       {}s", pi.uptime_secs("piggy").await);

    pi.stop("piggy").await?;
    println!("==> Smoke test passed.");
    Ok(())
}
