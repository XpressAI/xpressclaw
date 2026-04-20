//! container2wasm (c2w) runtime primitive (ADR-023).
//!
//! This module is the low-level primitive layer that hosts c2w-compiled
//! agent harness workloads. It wraps `wasmtime` with the configuration
//! we need: epoch interruption for rollback, WASI preview 1 for c2w
//! compatibility, and async execution so a running guest doesn't block
//! the Tokio runtime.
//!
//! The higher-level [`Harness`](crate::harness::Harness) implementation
//! (`C2wHarness`, arriving in ADR-023 task 3) composes this primitive
//! with the rest of the harness lifecycle (endpoint exposure, tmux
//! attach, snapshot/restore).
//!
//! # Layering
//!
//! ```text
//!  PiHarness (task 4) ─┐
//!                      ├─▶ C2wHarness (task 3) ─▶ C2wRuntime (this module) ─▶ wasmtime
//!  Built-in (task 1)  ─┘
//! ```

use std::path::Path;
use std::sync::Arc;

use tracing::{debug, warn};
use wasmtime::{Config, Engine, Module, Store};
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::{Error, Result};

/// Tick budget for wasmtime epoch interruption, in milliseconds.
///
/// The background epoch-tick task increments the engine's epoch counter
/// every `EPOCH_TICK_MS` milliseconds. Guests that exceed their
/// configured deadline are interrupted; this is the basis for both
/// cooperative scheduling and the rollback-on-failure guarantee.
pub const EPOCH_TICK_MS: u64 = 50;

/// Shared wasmtime engine + background epoch driver for the whole
/// xpressclaw process.
///
/// One `C2wRuntime` is constructed at server startup. Per-agent
/// [`C2wInstance`]s take an `Arc<C2wRuntime>` handle and create their
/// own `Store` against the shared `Engine` — this is cheap, isolates
/// per-agent guest state, and matches wasmtime's intended usage.
pub struct C2wRuntime {
    engine: Engine,
    _epoch_driver: tokio::task::JoinHandle<()>,
}

impl C2wRuntime {
    /// Build the process-wide c2w runtime.
    ///
    /// Spawns a background Tokio task that advances the engine's epoch
    /// counter every [`EPOCH_TICK_MS`] so guests can be deadline-interrupted.
    pub fn new() -> Result<Arc<Self>> {
        let mut config = Config::new();
        // Enable async support so guests can be driven cooperatively by
        // Tokio without blocking the executor (c2w agents will run for
        // long durations).
        config.async_support(true);
        // Epoch interruption is our rollback primitive — a misbehaving
        // guest (or one that's exceeded a per-step deadline) has its
        // execution aborted by ticking the epoch.
        config.epoch_interruption(true);
        // WASM features we need for c2w-compiled modules. c2w emits
        // standard WASI preview 1, so the defaults are sufficient.
        config.wasm_backtrace(true);

        let engine = Engine::new(&config)
            .map_err(|e| Error::Container(format!("failed to build wasmtime engine: {e}")))?;

        let engine_for_driver = engine.clone();
        let driver = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(EPOCH_TICK_MS));
            loop {
                interval.tick().await;
                engine_for_driver.increment_epoch();
            }
        });

        debug!(epoch_tick_ms = EPOCH_TICK_MS, "c2w runtime initialised");

        Ok(Arc::new(Self {
            engine,
            _epoch_driver: driver,
        }))
    }

    /// Access the shared wasmtime engine. Callers instantiating a guest
    /// should create a fresh `Store` against this engine so per-guest
    /// state is isolated.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Compile a WASM module from a file path. Modules can be cached and
    /// reused across many `Store`s / instances, so per-harness code
    /// typically holds the returned `Module` and re-instantiates it per
    /// agent launch.
    pub fn compile_module(&self, path: &Path) -> Result<Module> {
        let bytes = std::fs::read(path).map_err(|e| {
            Error::Container(format!("reading wasm module {}: {e}", path.display()))
        })?;
        Module::from_binary(&self.engine, &bytes)
            .map_err(|e| Error::Container(format!("compiling wasm module: {e}")))
    }
}

/// Specification for launching a guest instance.
#[derive(Debug, Clone)]
pub struct InstanceSpec {
    /// Environment variables to expose to the guest via WASI.
    pub env: Vec<(String, String)>,
    /// Preopened directories: `(host_path, guest_mount_path)`.
    ///
    /// The c2w-compiled guest sees these as its filesystem. Agent
    /// workspaces (per-agent scratch dir), the `xclaw` CLI socket path,
    /// and the harness image's rootfs are the three typical entries.
    pub preopens: Vec<(String, String)>,
    /// Maximum ticks of [`EPOCH_TICK_MS`] the guest may consume before
    /// an epoch-interruption aborts execution. `None` means no deadline
    /// (the guest runs until it returns or is explicitly stopped).
    pub deadline_ticks: Option<u64>,
    /// Command-line arguments passed to the guest's `_start` / `main`.
    pub args: Vec<String>,
}

impl Default for InstanceSpec {
    fn default() -> Self {
        Self {
            env: Vec::new(),
            preopens: Vec::new(),
            deadline_ticks: None,
            args: vec!["xpressclaw-guest".to_string()],
        }
    }
}

/// A running (or run-to-completion) guest instance.
///
/// In this MVP, [`C2wInstance::run_to_completion`] spawns the guest,
/// waits for it to exit, and returns stdout. Task 3 (C2wHarness) adds
/// long-lived instances with an HTTP endpoint exposed to the host.
pub struct C2wInstance {
    runtime: Arc<C2wRuntime>,
    module: Module,
    spec: InstanceSpec,
}

impl C2wInstance {
    /// Prepare a guest instance. No execution happens until `run_*` is
    /// called.
    pub fn new(runtime: Arc<C2wRuntime>, module: Module, spec: InstanceSpec) -> Self {
        Self {
            runtime,
            module,
            spec,
        }
    }

    /// Run the guest's `_start` entrypoint to completion. Blocks (async)
    /// until the guest returns or the deadline fires.
    ///
    /// Returns the exit code the guest requested via `wasi:exit`, or 0
    /// if it returned normally from `_start`.
    pub async fn run_to_completion(&self) -> Result<i32> {
        let mut builder = WasiCtxBuilder::new();
        builder.inherit_stdout().inherit_stderr();
        for (k, v) in &self.spec.env {
            builder.env(k, v);
        }
        for arg in &self.spec.args {
            builder.arg(arg);
        }
        for (host, guest) in &self.spec.preopens {
            builder
                .preopened_dir(
                    host,
                    guest,
                    wasmtime_wasi::DirPerms::all(),
                    wasmtime_wasi::FilePerms::all(),
                )
                .map_err(|e| Error::Container(format!("preopen {host} -> {guest}: {e}")))?;
        }
        let wasi: WasiP1Ctx = builder.build_p1();

        let mut store = Store::new(self.runtime.engine(), wasi);
        if let Some(ticks) = self.spec.deadline_ticks {
            store.set_epoch_deadline(ticks);
        }

        let mut linker = wasmtime::Linker::<WasiP1Ctx>::new(self.runtime.engine());
        preview1::add_to_linker_async(&mut linker, |s| s)
            .map_err(|e| Error::Container(format!("linking wasi preview 1: {e}")))?;

        let instance = linker
            .instantiate_async(&mut store, &self.module)
            .await
            .map_err(|e| Error::Container(format!("instantiate guest: {e}")))?;

        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| Error::Container(format!("no _start in guest: {e}")))?;

        match start.call_async(&mut store, ()).await {
            Ok(()) => Ok(0),
            Err(trap) => {
                // wasi:exit(n) surfaces as a trap with an I32Exit error;
                // any other trap is a genuine guest failure.
                if let Some(exit) = trap.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    Ok(exit.0)
                } else {
                    warn!(error = %trap, "c2w guest trapped");
                    Err(Error::Container(format!("guest trap: {trap}")))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_builds_with_epoch_driver() {
        let rt = C2wRuntime::new().expect("runtime builds");
        // Basic smoke check: engine exists and compile-a-bogus-module
        // returns an error rather than panicking.
        let bogus = wasmtime::Module::from_binary(rt.engine(), b"not wasm");
        assert!(bogus.is_err());
    }
}
