// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod tray;

use std::sync::Mutex;

use tauri::Manager;
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_PORT: u16 = 8935;

/// Holds the sidecar child process for cleanup on exit.
struct SidecarState(Mutex<Option<CommandChild>>);

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

            // Spawn the CLI binary as a sidecar
            let sidecar = app
                .shell()
                .sidecar("xpressclaw")
                .expect("failed to create sidecar command")
                .args(["up", "--port", &port.to_string(), "--workdir", &workdir]);

            let (mut rx, child) = sidecar.spawn().expect("failed to spawn sidecar");

            info!(pid = child.pid(), "sidecar spawned");

            // Store child handle for cleanup on exit
            let state = app.state::<SidecarState>();
            *state.0.lock().unwrap() = Some(child);

            // Forward sidecar stdout/stderr to our logs
            let handle_for_logs = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Some(event) = rx.recv().await {
                    match event {
                        CommandEvent::Stdout(line) => {
                            let msg = String::from_utf8_lossy(&line);
                            info!(target: "sidecar", "{}", msg.trim());
                        }
                        CommandEvent::Stderr(line) => {
                            let msg = String::from_utf8_lossy(&line);
                            info!(target: "sidecar", "{}", msg.trim());
                        }
                        CommandEvent::Terminated(payload) => {
                            error!(
                                code = ?payload.code,
                                signal = ?payload.signal,
                                "sidecar terminated"
                            );
                            if let Some(tray) = handle_for_logs.tray_by_id("main-tray") {
                                let _ = tray.set_tooltip(Some("xpressclaw - Server stopped"));
                            }
                        }
                        CommandEvent::Error(e) => {
                            error!(target: "sidecar", "error: {e}");
                        }
                        _ => {}
                    }
                }
            });

            // Wait for server to be ready, then show the window
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                wait_for_server(port).await;
                info!("server is ready");
                if let Some(window) = handle.get_webview_window("main") {
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
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                info!("app exiting, sidecar will be cleaned up by OS");
            }
        });
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
