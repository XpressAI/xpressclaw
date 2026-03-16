// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod tray;

use std::sync::Mutex;

use tauri::Manager;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_PORT: u16 = 8935;

/// Holds the sidecar child process for cleanup on exit.
struct SidecarState(Mutex<Option<std::process::Child>>);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let port = std::env::var("XPRESSCLAW_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init());

    // Prevent multiple instances on desktop
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(
            |app: &tauri::AppHandle, _args, _cwd| {
                if let Some(window) =
                    <tauri::AppHandle as tauri::Manager<tauri::Wry>>::get_webview_window(
                        app, "main",
                    )
                {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            },
        ));
    }

    builder
        .manage(SidecarState(Mutex::new(None)))
        .setup(move |app| {
            // Resolve working directory for the CLI sidecar
            let data_dir = dirs::home_dir()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
                .join(".xpressclaw");
            std::fs::create_dir_all(&data_dir).ok();
            let workdir = data_dir.to_string_lossy().to_string();

            // Resolve the sidecar binary path
            let sidecar_path = app
                .path()
                .resource_dir()
                .ok()
                .map(|d| d.join("binaries").join(sidecar_binary_name()))
                .filter(|p| p.exists())
                .or_else(|| {
                    // Dev mode: look in the binaries/ directory next to the Tauri manifest
                    let dev_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("binaries")
                        .join(sidecar_binary_name());
                    if dev_path.exists() {
                        Some(dev_path)
                    } else {
                        None
                    }
                })
                .expect("sidecar binary not found");

            info!(path = %sidecar_path.display(), "launching sidecar");

            // Spawn the sidecar process
            let mut cmd = std::process::Command::new(&sidecar_path);
            cmd.args(["up", "--port", &port.to_string(), "--workdir", &workdir])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());

            // On macOS, clear environment to avoid inheriting state that can
            // cause issues in the child process, then re-add essentials.
            #[cfg(target_os = "macos")]
            {
                cmd.env_clear()
                    .env("HOME", std::env::var("HOME").unwrap_or_default())
                    .env("PATH", std::env::var("PATH").unwrap_or_default())
                    .env("USER", std::env::var("USER").unwrap_or_default())
                    .env(
                        "RUST_LOG",
                        std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
                    );
            }

            // On Windows/Linux, inherit environment normally
            #[cfg(not(target_os = "macos"))]
            {
                cmd.env(
                    "RUST_LOG",
                    std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
                );
            }

            let child = cmd.spawn().expect("failed to spawn sidecar");

            info!(pid = child.id(), "sidecar spawned");

            // Store child handle for cleanup on exit
            let state = app.state::<SidecarState>();
            *state.0.lock().unwrap() = Some(child);

            // Wait for server to be ready, then show the window
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                wait_for_server(port).await;
                info!("server is ready");
                if let Some(window) = handle.get_webview_window("main") {
                    // Reload — the webview tried loading before the server was ready
                    let url = format!("http://localhost:{port}");
                    let _ = window.navigate(url.parse().unwrap());
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            });

            tray::setup_tray(app, port)?;
            info!(port, "xpressclaw desktop app started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_health,
            commands::get_server_port,
            commands::get_status,
            commands::open_browser,
        ])
        .build(tauri::generate_context!())
        .expect("error building xpressclaw desktop app")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let mut child = { app.state::<SidecarState>().0.lock().unwrap().take() };
                if let Some(ref mut child) = child {
                    info!("killing sidecar");
                    let _ = child.kill();
                }
            }
        });
}

/// Return the platform-specific sidecar binary name.
fn sidecar_binary_name() -> String {
    let triple = env!("TAURI_ENV_TARGET_TRIPLE");
    if cfg!(target_os = "windows") {
        format!("xpressclaw-{triple}.exe")
    } else {
        format!("xpressclaw-{triple}")
    }
}

/// Poll the health endpoint until the server is ready.
async fn wait_for_server(port: u16) {
    let url = format!("http://localhost:{port}/api/health");
    let client = reqwest::Client::new();

    for i in 0..120 {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => return,
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if i % 20 == 19 {
            info!("waiting for server to start...");
        }
    }
    warn!("server did not become ready within 60 seconds, showing window anyway");
}
