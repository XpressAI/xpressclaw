//! Managed-emulator lifecycle — the Android analog of `DockerManager`'s
//! "installed? / running? / if not, spawn it". The setup routes surface this
//! the same way `check_docker` / `start_docker` do. See ADR-024.
//!
//! - **installed** → [`crate::android::sdk::detect`]`().ready()` (SDK + emulator
//!   binary + a system image + an AVD).
//! - **running**   → [`is_running`] (a device is up and finished booting).
//! - **start**     → [`start`] (spawn the emulator, detached).

use std::path::PathBuf;
use std::process::Command;

use crate::android::sdk;
use crate::error::{Error, Result};

/// Standard serial for a single managed emulator.
pub const DEFAULT_SERIAL: &str = "emulator-5554";

/// Whether an emulator/device is up and finished booting — the analog of
/// `DockerManager::connect().is_ok()`. Uses `adb_client`, which is synchronous,
/// so call this inside `spawn_blocking`.
pub fn is_running(serial: &str) -> bool {
    match crate::android::AndroidDevice::via_server(serial) {
        Ok(mut d) => d
            .shell("getprop sys.boot_completed")
            .map(|s| s.trim() == "1")
            .unwrap_or(false),
        Err(_) => false, // no adb server / no device → not running
    }
}

/// Path to the SDK's `emulator` binary, if installed.
pub fn emulator_binary() -> Option<PathBuf> {
    let root = sdk::find_sdk_root()?;
    let name = if cfg!(windows) { "emulator.exe" } else { "emulator" };
    let p = root.join("emulator").join(name);
    p.exists().then_some(p)
}

/// Launch the emulator for `avd`, detached — the analog of
/// `start_docker_desktop`. Returns once the launch is issued; booting is async
/// (poll [`is_running`] afterward).
pub fn start(avd: &str) -> Result<()> {
    let emu = emulator_binary().ok_or_else(|| {
        Error::Android("emulator binary not found — install the Android SDK".to_string())
    })?;
    let emu_str = emu.to_string_lossy().to_string();

    // Detach so the emulator outlives this process (same intent as
    // start_docker_desktop launching Docker Desktop independently).
    #[cfg(target_os = "windows")]
    let spawned = Command::new("cmd")
        .args(["/c", "start", "", &emu_str, "-avd", avd, "-no-boot-anim"])
        .spawn();
    #[cfg(not(target_os = "windows"))]
    let spawned = Command::new(&emu)
        .args(["-avd", avd, "-no-boot-anim"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    spawned
        .map(|_| ())
        .map_err(|e| Error::Android(format!("failed to launch emulator '{avd}': {e}")))
}
