// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod tray;

use std::sync::Mutex;

use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
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

    // On macOS, disable the default menu so we can replace the Quit item
    // with a custom one that shows a confirmation dialog. The default Quit
    // menu item calls std::process::exit(0) directly, bypassing all event
    // handlers (ExitRequested never fires on macOS Cmd-Q).
    #[cfg(target_os = "macos")]
    {
        builder = builder.enable_macos_default_menu(false);
    }

    builder
        .manage(SidecarState(Mutex::new(None)))
        // Window close (Cmd-W / red X) → hide to tray instead of quitting.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                info!("window hidden to tray");
            }
        })
        // Handle our custom "quit" menu item (Cmd-Q on macOS)
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "custom-quit" {
                confirm_quit(app);
            }
        })
        .setup(move |app| {
            // Build the custom macOS app menu with our own Quit item
            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{
                    MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
                };

                let quit_item = MenuItemBuilder::with_id("custom-quit", "Quit xpressclaw")
                    .accelerator("CmdOrCtrl+Q")
                    .build(app)?;

                let app_submenu = SubmenuBuilder::new(app, "xpressclaw")
                    .about(None)
                    .separator()
                    .items(&[&PredefinedMenuItem::hide(app, None)?])
                    .items(&[&PredefinedMenuItem::hide_others(app, None)?])
                    .items(&[&PredefinedMenuItem::show_all(app, None)?])
                    .separator()
                    .item(&quit_item)
                    .build()?;

                let edit_submenu = SubmenuBuilder::new(app, "Edit")
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;

                let menu = MenuBuilder::new(app)
                    .items(&[&app_submenu, &edit_submenu])
                    .build()?;

                app.set_menu(menu)?;
            }

            // Resolve working directory for the CLI sidecar
            let data_dir = dirs::home_dir()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
                .join(".xpressclaw");
            std::fs::create_dir_all(&data_dir).ok();
            let workdir = data_dir.to_string_lossy().to_string();

            // Resolve the sidecar binary path.
            let cli_name = if cfg!(target_os = "windows") {
                "xpressclaw.exe"
            } else {
                "xpressclaw"
            };
            let sidecar_name = sidecar_binary_name();
            let sidecar_path = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
                .and_then(|d| {
                    let flat = d.join(cli_name);
                    if flat.exists() {
                        return Some(flat);
                    }
                    let with_triple = d.join(&sidecar_name);
                    if with_triple.exists() {
                        return Some(with_triple);
                    }
                    None
                })
                .or_else(|| {
                    app.path().resource_dir().ok().and_then(|d| {
                        let flat = d.join(cli_name);
                        if flat.exists() {
                            return Some(flat);
                        }
                        let with_triple = d.join(&sidecar_name);
                        if with_triple.exists() {
                            return Some(with_triple);
                        }
                        let in_subdir = d.join("binaries").join(&sidecar_name);
                        if in_subdir.exists() {
                            return Some(in_subdir);
                        }
                        None
                    })
                })
                .or_else(|| {
                    let dev_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("binaries")
                        .join(&sidecar_name);
                    if dev_path.exists() {
                        Some(dev_path)
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| {
                    warn!(
                        sidecar_name,
                        "sidecar binary not found in app bundle, will try PATH"
                    );
                    std::path::PathBuf::from(cli_name)
                });

            info!(path = %sidecar_path.display(), "launching sidecar");

            // Spawn the sidecar process
            let mut cmd = std::process::Command::new(&sidecar_path);
            cmd.args(["up", "--port", &port.to_string(), "--workdir", &workdir])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());

            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }

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

            #[cfg(not(target_os = "macos"))]
            {
                cmd.env(
                    "RUST_LOG",
                    std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
                );
            }

            let child = match cmd.spawn() {
                Ok(child) => child,
                Err(e) => {
                    warn!(
                        error = %e,
                        path = %sidecar_path.display(),
                        "failed to spawn sidecar — the app will start but the server won't be running"
                    );
                    tray::setup_tray(app, port)?;
                    return Ok(());
                }
            };

            info!(pid = child.id(), "sidecar spawned");

            let state = app.state::<SidecarState>();
            *state.0.lock().unwrap() = Some(child);

            // Wait for server to be ready, then show the window
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                wait_for_server(port).await;
                info!("server is ready");
                if let Some(window) = handle.get_webview_window("main") {
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
            // Safety net: if the process is killed through means we can't
            // intercept (dock quit, SIGTERM), at least clean up the sidecar.
            if let tauri::RunEvent::Exit = event {
                shutdown_sidecar(app);
            }
        });
}

/// Show a confirmation dialog, then shut down if confirmed.
/// Used by both the custom Cmd-Q menu item and the tray Quit button.
pub fn confirm_quit(app: &tauri::AppHandle) {
    let handle = app.clone();
    app.dialog()
        .message("Your agents will stop running and won't be available until you restart.")
        .title("Quit xpressclaw?")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Quit".into(),
            "Cancel".into(),
        ))
        .show(move |confirmed| {
            if confirmed {
                info!("quit confirmed — shutting down");
                shutdown_sidecar(&handle);
                std::process::exit(0);
            }
        });
}

/// Gracefully stop agents and kill the sidecar.
/// Runs `xpressclaw down` (which stops Docker containers via the API),
/// then kills the sidecar child process.
pub fn shutdown_sidecar(app: &tauri::AppHandle) {
    let port = std::env::var("XPRESSCLAW_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    // Resolve the sidecar binary path from the stored child's info,
    // or fall back to the CLI name on PATH.
    let cli_name = if cfg!(target_os = "windows") {
        "xpressclaw.exe"
    } else {
        "xpressclaw"
    };
    let sidecar_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(cli_name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from(cli_name));

    // Run `xpressclaw down --port <port>` to stop all agents/containers
    info!("running xpressclaw down");
    let _ = std::process::Command::new(&sidecar_path)
        .args(["down", "--port", &port.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Kill the sidecar server process
    let mut child = app.state::<SidecarState>().0.lock().unwrap().take();
    if let Some(ref mut child) = child {
        info!("killing sidecar process");
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn sidecar_binary_name() -> String {
    let triple = env!("TAURI_ENV_TARGET_TRIPLE");
    if cfg!(target_os = "windows") {
        format!("xpressclaw-{triple}.exe")
    } else {
        format!("xpressclaw-{triple}")
    }
}

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
