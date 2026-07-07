//! Android SDK / emulator install detection — the preflight for the managed
//! emulator (device control itself needs no SDK; adb_client is pure Rust).
//! Reports what's present so the CLI / UI can guide setup. See ADR-024.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// Default system image for managed emulators: Google Play (Store + sign-in).
/// A production build, so no `adb root` — the control path needs none (ADR-024).
pub const DEFAULT_SYSTEM_IMAGE: &str = "system-images;android-36;google_apis_playstore;x86_64";

/// Default device profile for managed emulators.
pub const DEFAULT_DEVICE_PROFILE: &str = "pixel_6";

/// Snapshot of the local Android SDK install relevant to managed emulators.
#[derive(Debug, Clone, Serialize)]
pub struct SdkStatus {
    /// Resolved SDK root, if found.
    pub sdk_root: Option<String>,
    /// `emulator` binary present.
    pub emulator: bool,
    /// `cmdline-tools/latest` (sdkmanager/avdmanager) present.
    pub cmdline_tools: bool,
    /// `platform-tools` (adb) present.
    pub platform_tools: bool,
    /// Installed system images, as `api/tag/abi`.
    pub system_images: Vec<String>,
    /// Created AVD names.
    pub avds: Vec<String>,
    /// Hardware acceleration available (`None` if it couldn't be checked).
    pub accel_ok: Option<bool>,
}

impl SdkStatus {
    /// Whether a managed emulator can be booted right now.
    pub fn ready(&self) -> bool {
        self.sdk_root.is_some()
            && self.emulator
            && !self.system_images.is_empty()
            && !self.avds.is_empty()
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn bat(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.bat")
    } else {
        name.to_string()
    }
}

/// Locate the Android SDK root from env vars, then platform defaults.
pub fn find_sdk_root() -> Option<PathBuf> {
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(p) = std::env::var_os(var) {
            let path = PathBuf::from(p);
            if path.is_dir() {
                return Some(path);
            }
        }
    }
    let home = home_dir()?;
    [
        home.join("AppData").join("Local").join("Android").join("Sdk"), // Windows
        home.join("Library").join("Android").join("sdk"),               // macOS
        home.join("Android").join("Sdk"),                               // Linux
    ]
    .into_iter()
    .find(|p| p.is_dir())
}

/// List installed system images under `<sdk>/system-images/<api>/<tag>/<abi>`.
fn list_system_images(sdk: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(apis) = std::fs::read_dir(sdk.join("system-images")) else {
        return out;
    };
    for api in apis.flatten().filter(|e| e.path().is_dir()) {
        let api_name = api.file_name().to_string_lossy().into_owned();
        let Ok(tags) = std::fs::read_dir(api.path()) else {
            continue;
        };
        for tag in tags.flatten().filter(|e| e.path().is_dir()) {
            let tag_name = tag.file_name().to_string_lossy().into_owned();
            let Ok(abis) = std::fs::read_dir(tag.path()) else {
                continue;
            };
            for abi in abis.flatten().filter(|e| e.path().is_dir()) {
                let abi_name = abi.file_name().to_string_lossy().into_owned();
                out.push(format!("{api_name}/{tag_name}/{abi_name}"));
            }
        }
    }
    out
}

/// List AVDs via `emulator -list-avds`, falling back to scanning `~/.android/avd`.
fn list_avds(sdk: &Path) -> Vec<String> {
    let emu = sdk.join("emulator").join(exe("emulator"));
    if emu.exists() {
        if let Ok(out) = Command::new(&emu).arg("-list-avds").output() {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
            }
        }
    }
    let mut out = Vec::new();
    if let Some(home) = home_dir() {
        if let Ok(entries) = std::fs::read_dir(home.join(".android").join("avd")) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Some(stripped) = name.strip_suffix(".avd") {
                    out.push(stripped.to_string());
                }
            }
        }
    }
    out
}

/// Check hardware acceleration via `emulator -accel-check`.
fn check_accel(sdk: &Path) -> Option<bool> {
    let emu = sdk.join("emulator").join(exe("emulator"));
    if !emu.exists() {
        return None;
    }
    Command::new(&emu)
        .arg("-accel-check")
        .output()
        .ok()
        .map(|out| out.status.success())
}

/// Inspect the local Android SDK install.
pub fn detect() -> SdkStatus {
    let Some(root) = find_sdk_root() else {
        return SdkStatus {
            sdk_root: None,
            emulator: false,
            cmdline_tools: false,
            platform_tools: false,
            system_images: Vec::new(),
            avds: Vec::new(),
            accel_ok: None,
        };
    };

    let emulator = root.join("emulator").join(exe("emulator")).exists();
    let cmdline_tools = root
        .join("cmdline-tools")
        .join("latest")
        .join("bin")
        .join(bat("sdkmanager"))
        .exists();
    let platform_tools = root.join("platform-tools").join(exe("adb")).exists();
    let system_images = list_system_images(&root);
    let avds = list_avds(&root);
    let accel_ok = if emulator { check_accel(&root) } else { None };

    SdkStatus {
        sdk_root: Some(root.to_string_lossy().into_owned()),
        emulator,
        cmdline_tools,
        platform_tools,
        system_images,
        avds,
        accel_ok,
    }
}
