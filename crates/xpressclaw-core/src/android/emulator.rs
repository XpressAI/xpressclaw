//! Managed-emulator lifecycle: installed? ([`crate::android::sdk::detect`]),
//! running? ([`is_running`]), spawn it ([`start`]) — the Android analog of the
//! Docker check/start flow. See ADR-024.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::android::sdk;
use crate::error::{Error, Result};

/// Standard serial for a single managed emulator.
pub const DEFAULT_SERIAL: &str = "emulator-5554";

/// Whether an emulator/device is up and finished booting. Blocking
/// (adb_client is synchronous) — call inside `spawn_blocking`.
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

/// Launch the emulator for `avd`, detached. Returns once the launch is issued;
/// booting is async — poll [`is_running`] afterward.
pub fn start(avd: &str) -> Result<()> {
    // AVD names are [A-Za-z0-9._-]; reject shell metacharacters before the name
    // reaches any command line (defense in depth — every start path funnels here).
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

    // Keep the emulator's diagnostics on disk: booting is async, so this log is
    // the only record of a failure that happens seconds after spawn.
    let log_path = std::env::temp_dir().join("xpressclaw-emulator.log");
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok((a, b)) => (Stdio::from(a), Stdio::from(b)),
        Err(_) => (Stdio::null(), Stdio::null()),
    };

    // Spawn the binary directly (no shell layer) so a launch failure is a real
    // spawn error; detach so the emulator outlives this process.
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
        // Own process group: don't catch SIGINT/SIGHUP aimed at the server's terminal.
        cmd.process_group(0);
    }

    cmd.spawn()
        .map_err(|e| Error::Android(format!("failed to launch emulator '{avd}': {e}")))?;
    tracing::info!(avd, log = %log_path.display(), "launched android emulator");
    Ok(())
}
