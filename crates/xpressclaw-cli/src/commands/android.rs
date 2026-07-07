//! `xpressclaw android` — drive an Android device/emulator directly over adb
//! (pure Rust via `adb_client`, no `adb` binary). See ADR-024.

use std::net::SocketAddr;
use std::path::Path;

use clap::Subcommand;
use xpressclaw_core::android::{emulator, AndroidDevice};
use xpressclaw_core::config::Config;

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

    /// Long-press (touch and hold) at coordinates
    LongPress {
        x: i32,
        y: i32,
        /// Hold duration in ms
        #[arg(default_value_t = 600)]
        ms: u32,
    },

    /// Launch an app by package name (e.g. com.android.settings)
    OpenApp {
        /// Package name
        package: String,
    },

    /// Swipe from (x1,y1) to (x2,y2)
    Swipe {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        /// Duration in ms
        #[arg(default_value_t = 300)]
        ms: u32,
    },

    /// Type text into the focused field (tap it to focus first)
    Type {
        /// Text to type
        text: String,
    },

    /// Send a key event, e.g. KEYCODE_BACK, KEYCODE_HOME, KEYCODE_ENTER
    Key {
        /// Keycode name or numeric code
        key: String,
    },

    /// Print the compact screen map (labeled/clickable elements + tap centers) as JSON
    ScreenMap,

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
    serial: Option<String>,
    tcp: Option<String>,
) -> anyhow::Result<()> {
    // `doctor` inspects the local SDK install, not a device — no connection.
    if let AndroidCommand::Doctor = command {
        print_doctor();
        return Ok(());
    }

    // Connect: explicit --tcp wins (direct to adbd, no adb server), then
    // --serial (via the adb server). With neither flag, fall back to the
    // workspace config's `android` section — the SAME target the server and
    // agents drive — and finally the managed emulator's default serial.
    let mut device = if let Some(addr) = tcp {
        let socket: SocketAddr = addr
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid --tcp address '{addr}': {e}"))?;
        AndroidDevice::via_tcp(socket)?
    } else if let Some(serial) = serial {
        AndroidDevice::via_server(&serial)?
    } else {
        let android = Config::load(Path::new("xpressclaw.yaml"))
            .map(|c| c.android)
            .unwrap_or_default();
        if let Some(addr) = android.tcp.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let socket: SocketAddr = addr.parse().map_err(|e| {
                anyhow::anyhow!("invalid android.tcp '{addr}' in xpressclaw.yaml: {e}")
            })?;
            AndroidDevice::via_tcp(socket)?
        } else if let Some(s) = android
            .serial
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            AndroidDevice::via_server(s)?
        } else {
            AndroidDevice::via_server(emulator::DEFAULT_SERIAL)?
        }
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
        AndroidCommand::LongPress { x, y, ms } => {
            device.long_press(x, y, ms)?;
            println!("Long-pressed ({x}, {y}) for {ms}ms");
        }
        AndroidCommand::OpenApp { package } => {
            device.open_app(&package)?;
            println!("Launched {package}");
        }
        AndroidCommand::Swipe { x1, y1, x2, y2, ms } => {
            device.swipe(x1, y1, x2, y2, ms)?;
            println!("Swiped ({x1}, {y1}) → ({x2}, {y2}) over {ms}ms");
        }
        AndroidCommand::Type { text } => {
            device.input_text(&text)?;
            println!("Typed: {text}");
        }
        AndroidCommand::Key { key } => {
            device.key_event(&key)?;
            println!("Sent {key}");
        }
        AndroidCommand::ScreenMap => {
            let elements = device.screen_elements()?;
            println!("{}", serde_json::to_string_pretty(&elements)?);
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
