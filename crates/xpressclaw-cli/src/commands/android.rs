//! `xpressclaw android` — drive an Android device/emulator directly over adb
//! (pure Rust via `adb_client`, no `adb` binary). Feature-gated behind
//! `android`; see ADR-024.

use std::net::SocketAddr;

use clap::Subcommand;
use xpressclaw_core::android::AndroidDevice;

#[derive(Subcommand)]
pub enum AndroidCommand {
    /// Capture a screenshot to a PNG file
    Screenshot {
        /// Output PNG path
        #[arg(default_value = "screen.png")]
        out: String,
    },

    /// Tap absolute device coordinates
    Tap { x: i32, y: i32 },

    /// Find a UI element by text / content-desc and tap its center
    TapText {
        /// Visible text or content-description of the element
        label: String,
    },

    /// Dump the UI accessibility tree (uiautomator) as XML
    Dump,

    /// Run an adb shell command and print its stdout
    Shell {
        /// Command and arguments, e.g. `getprop sys.boot_completed`
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    /// Check the Android SDK install (emulator, system images, AVDs) needed
    /// for the managed-emulator path
    Doctor,
}

pub async fn run(
    command: AndroidCommand,
    serial: String,
    tcp: Option<String>,
) -> anyhow::Result<()> {
    // `doctor` inspects the local SDK install, not a device — no connection.
    if let AndroidCommand::Doctor = command {
        print_doctor();
        return Ok(());
    }

    // Connect: direct to adbd over TCP if --tcp was given (no adb server),
    // otherwise through the adb server by serial.
    let mut device = match tcp {
        Some(addr) => {
            let socket: SocketAddr = addr
                .parse()
                .map_err(|e| anyhow::anyhow!("invalid --tcp address '{addr}': {e}"))?;
            AndroidDevice::via_tcp(socket)?
        }
        None => AndroidDevice::via_server(&serial)?,
    };

    match command {
        AndroidCommand::Screenshot { out } => {
            device.screenshot_to(&out)?;
            println!("Saved screenshot to {out}");
        }
        AndroidCommand::Tap { x, y } => {
            device.tap(x, y)?;
            println!("Tapped ({x}, {y})");
        }
        AndroidCommand::TapText { label } => {
            let (x, y) = device.tap_text(&label)?;
            println!("Tapped '{label}' at ({x}, {y})");
        }
        AndroidCommand::Dump => {
            let xml = device.ui_dump()?;
            println!("{xml}");
        }
        AndroidCommand::Shell { command } => {
            let cmd = command.join(" ");
            let out = device.shell(&cmd)?;
            print!("{out}");
        }
        AndroidCommand::Doctor => unreachable!("handled before connecting"),
    }

    Ok(())
}

/// Print an SDK preflight report for the managed-emulator path.
fn print_doctor() {
    let s = xpressclaw_core::android::sdk::detect();

    println!("Android managed-emulator preflight\n");

    let Some(root) = &s.sdk_root else {
        println!("  [MISSING]  Android SDK not found");
        println!("             Set ANDROID_HOME, or install via Android Studio / cmdline-tools.");
        return;
    };
    println!("  [ok]       SDK root: {root}");

    let mark = |b: bool| if b { "[ok]     " } else { "[MISSING]" };
    println!("  {} emulator binary", mark(s.emulator));
    println!(
        "  {} cmdline-tools (sdkmanager/avdmanager)",
        mark(s.cmdline_tools)
    );
    println!("  {} platform-tools (adb)", mark(s.platform_tools));

    use xpressclaw_core::android::sdk::{DEFAULT_DEVICE_PROFILE, DEFAULT_SYSTEM_IMAGE};
    if s.system_images.is_empty() {
        println!("  [MISSING]  system images");
        println!("             e.g. sdkmanager \"{DEFAULT_SYSTEM_IMAGE}\"");
    } else {
        println!("  [ok]       system images ({}):", s.system_images.len());
        for img in &s.system_images {
            println!("                 {img}");
        }
    }

    if s.avds.is_empty() {
        println!("  [MISSING]  AVDs");
        println!("             e.g. avdmanager create avd -n android \\");
        println!("                    -k \"{DEFAULT_SYSTEM_IMAGE}\" -d {DEFAULT_DEVICE_PROFILE}");
    } else {
        println!("  [ok]       AVDs ({}): {}", s.avds.len(), s.avds.join(", "));
    }

    match s.accel_ok {
        Some(true) => println!("  [ok]       hardware acceleration available"),
        Some(false) => {
            println!("  [WARN]     hardware acceleration NOT available — emulator will be slow")
        }
        None => println!("  [?]        hardware acceleration: could not check"),
    }

    println!();
    if s.ready() {
        println!("Ready — a managed emulator can be booted.");
    } else {
        println!("Not ready — resolve the [MISSING] items above.");
    }
}
