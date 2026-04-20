//! `xpressclaw c2w-smoke` — end-to-end smoke test for the c2w runtime.
//!
//! Exists so the c2w runtime + harness integration can be exercised on
//! a real machine without writing test scaffolding, during ADR-023
//! implementation. Launches a minimal WASI guest that prints a line to
//! stdout, asserts it exited, and reports the result.
//!
//! Will be removed once the pi harness (ADR-023 task 4) is ready — it
//! subsumes this as a more realistic end-to-end smoke.

use std::sync::Arc;
use std::time::Duration;

use tempfile::NamedTempFile;
use xpressclaw_core::c2w::C2wRuntime;
use xpressclaw_core::harness::types::ContainerSpec;
use xpressclaw_core::harness::{C2wHarness, Harness};

/// A WASI preview-1 guest that immediately returns from `_start`.
///
/// The point of this smoke test is the harness lifecycle, not WASI
/// feature coverage — a no-op guest proves launch/run/exit/stop work
/// without hand-crafting a string-printing module. Real guest images
/// ship with the pi harness (task 4).
const NOOP_WASM_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "_start")))
"#;

pub async fn run() -> anyhow::Result<()> {
    println!("==> Building wasmtime engine + epoch driver...");
    let runtime = C2wRuntime::new()?;

    println!("==> Compiling minimal WASI guest WAT -> WASM...");
    let wasm = wat::parse_str(NOOP_WASM_WAT)?;
    let tmp = NamedTempFile::new()?;
    std::fs::write(tmp.path(), &wasm)?;

    let cache = std::env::temp_dir().join("xpressclaw-c2w-smoke-cache");
    let harness: Arc<dyn Harness> = Arc::new(C2wHarness::new(runtime, cache));

    let spec = ContainerSpec {
        image: tmp.path().to_string_lossy().into_owned(),
        ..ContainerSpec::default()
    };

    println!("==> Launching guest as agent 'smoke'...");
    let info = harness.launch("smoke", &spec).await?;
    println!("    container_id: {}", info.container_id);
    println!("    status:       {}", info.status);
    println!("    kind:         {}", harness.kind());

    println!("==> Guest output follows:");
    println!("----------------------------------------");

    for _ in 0..100 {
        if !harness.is_running("smoke").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    println!("----------------------------------------");

    let list = harness.list().await?;
    if list.is_empty() {
        anyhow::bail!("expected 1 agent in list after launch, got 0");
    }
    let running = &list[0];
    println!("==> Post-run list entry:");
    println!("    agent_id: {}", running.agent_id);
    println!("    status:   {}", running.status);
    println!("    uptime:   {}s", harness.uptime_secs("smoke").await);

    if running.status != "exited" {
        anyhow::bail!(
            "expected guest to have exited, status is {:?}",
            running.status
        );
    }

    harness.stop("smoke").await?;
    let after = harness.list().await?;
    if !after.is_empty() {
        anyhow::bail!(
            "expected registry empty after stop, got {} entries",
            after.len()
        );
    }

    println!("==> Smoke test passed.");
    Ok(())
}
