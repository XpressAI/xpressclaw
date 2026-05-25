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
}

pub async fn run(
    command: AndroidCommand,
    serial: String,
    tcp: Option<String>,
) -> anyhow::Result<()> {
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
    }

    Ok(())
}
