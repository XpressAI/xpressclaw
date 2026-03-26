use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::sync::Mutex;
use tracing::info;

use crate::state::AppState;

/// Chrome debug port — exposed to containers via host.docker.internal
const CHROME_DEBUG_PORT: u16 = 9222;

/// Track the Chrome process
static CHROME_PROCESS: OnceLock<Mutex<Option<tokio::process::Child>>> = OnceLock::new();

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/launch", post(launch_chrome))
        .route("/status", get(chrome_status))
        .route("/stop", post(stop_chrome))
}

/// Launch Chrome with remote debugging enabled.
/// Agents connect via CDP from inside their containers.
async fn launch_chrome(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mutex = CHROME_PROCESS.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().await;

    // Check if already running
    if let Some(ref mut child) = *guard {
        match child.try_wait() {
            Ok(None) => {
                // Still running
                return Ok(Json(json!({
                    "status": "already_running",
                    "cdp_url": format!("http://host.docker.internal:{CHROME_DEBUG_PORT}"),
                    "port": CHROME_DEBUG_PORT,
                })));
            }
            _ => {
                // Dead, will relaunch
            }
        }
    }

    // Find Chrome binary
    let chrome_path = find_chrome();
    let Some(chrome) = chrome_path else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(
                json!({ "error": "Chrome/Chromium not found. Install Google Chrome or Chromium." }),
            ),
        ));
    };

    info!(chrome = %chrome, port = CHROME_DEBUG_PORT, "launching Chrome with remote debugging");

    // Use a separate user data directory so this Chrome instance is
    // independent of any existing Chrome windows. Without this, Chrome
    // reuses the existing process and ignores --remote-debugging-port.
    let data_dir = state
        .config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("chrome-debug-profile");
    let _ = std::fs::create_dir_all(&data_dir);

    let child = tokio::process::Command::new(&chrome)
        .args([
            &format!("--remote-debugging-port={CHROME_DEBUG_PORT}"),
            &format!("--user-data-dir={}", data_dir.to_string_lossy()),
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-timer-throttling",
            "--disable-backgrounding-occluded-windows",
            "--disable-renderer-backgrounding",
            // Allow CDP connections from Docker containers (host.docker.internal)
            "--remote-allow-origins=*",
            "about:blank",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to launch Chrome: {e}") })),
            )
        })?;

    *guard = Some(child);

    // Give Chrome a moment to start the debug server
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    Ok(Json(json!({
        "status": "launched",
        "cdp_url": format!("http://host.docker.internal:{CHROME_DEBUG_PORT}"),
        "port": CHROME_DEBUG_PORT,
    })))
}

/// Check if Chrome is running with debug port.
async fn chrome_status() -> Json<Value> {
    let mutex = CHROME_PROCESS.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().await;

    if let Some(ref mut child) = *guard {
        match child.try_wait() {
            Ok(None) => {
                return Json(json!({
                    "running": true,
                    "cdp_url": format!("http://host.docker.internal:{CHROME_DEBUG_PORT}"),
                    "port": CHROME_DEBUG_PORT,
                }));
            }
            _ => {
                *guard = None;
            }
        }
    }

    Json(json!({ "running": false }))
}

/// Stop Chrome.
async fn stop_chrome() -> Json<Value> {
    let mutex = CHROME_PROCESS.get_or_init(|| Mutex::new(None));
    let mut guard = mutex.lock().await;

    if let Some(ref mut child) = *guard {
        let _ = child.kill().await;
        info!("stopped Chrome");
    }
    *guard = None;

    Json(json!({ "stopped": true }))
}

/// Find Chrome/Chromium binary on the host.
fn find_chrome() -> Option<String> {
    let candidates = match std::env::consts::OS {
        "macos" => vec![
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        ],
        "windows" => vec![
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ],
        "linux" => vec![
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ],
        _ => vec![],
    };

    for path in candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.to_string());
        }
        // On Linux, check PATH
        if std::env::consts::OS == "linux" {
            if let Ok(output) = std::process::Command::new("which").arg(path).output() {
                if output.status.success() {
                    return Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
                }
            }
        }
    }

    None
}
