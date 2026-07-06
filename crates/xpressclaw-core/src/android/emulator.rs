//! Managed-emulator lifecycle — the Android analog of `DockerManager`'s
//! "installed? / running? / if not, spawn it". The setup routes surface this
//! the same way `check_docker` / `start_docker` do. See ADR-024.
//!
//! - **installed** → [`crate::android::sdk::detect`]`().ready()` (SDK + emulator
//!   binary + a system image + an AVD).
//! - **running**   → [`is_running`] (a device is up and finished booting).
//! - **start**     → [`start`] (spawn the emulator, detached).

use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    // Validate before this string reaches a shell. On Windows it is handed to
    // `cmd /c start`, and cmd.exe re-parses the command line with its own quoting
    // rules (a documented std caveat) — so metacharacters like `&`, `|`, or `"`
    // in the AVD name could otherwise break out of the argument and run arbitrary
    // commands on the HOST. AVD names are `[A-Za-z0-9._-]`; reject anything else.
    if avd.is_empty()
        || !avd
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(Error::Android(format!(
            "invalid AVD name {avd:?} (expected characters in [A-Za-z0-9._-])"
        )));
    }

    let emu = emulator_binary().ok_or_else(|| {
        Error::Android("emulator binary not found — install the Android SDK".to_string())
    })?;

    // Capture the emulator's own diagnostics (bad AVD, missing WHPX/KVM
    // acceleration) to a log file instead of discarding them. Booting is async so
    // we can't await a failure that happens seconds after spawn — but with the
    // output on disk the reason is at least recoverable. Best-effort: fall back to
    // null if the log can't be opened.
    let log_path = std::env::temp_dir().join("xpressclaw-emulator.log");
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok((a, b)) => (Stdio::from(a), Stdio::from(b)),
        Err(_) => (Stdio::null(), Stdio::null()),
    };

    // Spawn the emulator binary directly (no `cmd /c start` shell layer) so a
    // launch failure surfaces as a real spawn error, and detach it so it outlives
    // this process. On Windows CREATE_NO_WINDOW detaches without a console window
    // (the same approach as the Tauri sidecar launch); the emulator draws its own
    // GUI window.
    let mut cmd = Command::new(&emu);
    cmd.args(["-avd", avd, "-no-boot-anim"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group so the emulator doesn't catch SIGINT/SIGHUP aimed at
        // the server's controlling terminal (the analog of the Windows detach).
        cmd.process_group(0);
    }

    cmd.spawn()
        .map_err(|e| Error::Android(format!("failed to launch emulator '{avd}': {e}")))?;
    tracing::info!(avd, log = %log_path.display(), "launched android emulator");
    Ok(())
}
