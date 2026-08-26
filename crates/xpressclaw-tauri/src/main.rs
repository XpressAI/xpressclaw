// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod profiles;
mod tray;

use std::sync::Mutex;
use std::{net::IpAddr, path::PathBuf};

use serde::Deserialize;
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_PORT: u16 = 8935;
const DEV_FRONTEND_PORT: u16 = 5173;
// Anonymous stdout protocol emitted by the CLI when the local sidecar needs
// a per-start login token. The matched line is consumed and never logged.
const STARTUP_TOKEN_PREFIX: &str = "XPRESSCLAW_STARTUP_TOKEN=";

#[derive(Debug, Clone)]
struct LocalInstanceConnection {
    port: u16,
    url: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct DesktopYaml {
    instance: DesktopListener,
}

#[derive(Deserialize)]
#[serde(default)]
struct DesktopListener {
    bind: IpAddr,
    port: u16,
}

impl Default for DesktopListener {
    fn default() -> Self {
        Self {
            bind: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: DEFAULT_PORT,
        }
    }
}

fn local_instance_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join(".xpressclaw")
}

fn local_instance_connection() -> LocalInstanceConnection {
    let config_path = local_instance_root().join("xpressclaw.yaml");
    let saved = match std::fs::read_to_string(&config_path) {
        Ok(yaml) => serde_yaml::from_str::<DesktopYaml>(&yaml).unwrap_or_else(|error| {
            warn!(path = %config_path.display(), %error, "could not read the saved Desktop listener; using safe defaults");
            DesktopYaml::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DesktopYaml::default(),
        Err(error) => {
            warn!(path = %config_path.display(), %error, "could not read the saved Desktop listener; using safe defaults");
            DesktopYaml::default()
        }
    };
    let port = std::env::var("XPRESSCLAW_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(saved.instance.port);
    LocalInstanceConnection {
        port,
        url: listener_client_url(saved.instance.bind, port),
    }
}

pub(crate) fn local_server_port() -> u16 {
    local_instance_connection().port
}

fn listener_client_url(bind: IpAddr, port: u16) -> String {
    match bind {
        IpAddr::V4(address) if address.is_unspecified() => format!("http://localhost:{port}"),
        IpAddr::V4(address) if address == std::net::Ipv4Addr::LOCALHOST => {
            format!("http://localhost:{port}")
        }
        IpAddr::V4(address) => format!("http://{address}:{port}"),
        IpAddr::V6(address) if address.is_unspecified() => format!("http://[::1]:{port}"),
        IpAddr::V6(address) => format!("http://[{address}]:{port}"),
    }
}

/// Holds the sidecar child process for cleanup on exit.
struct SidecarState(Mutex<Option<std::process::Child>>);

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let local_connection = local_instance_connection();
    let port = local_connection.port;
    let local_url = local_connection.url.clone();

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
        // Window close (Cmd-W / red X) hides the main window to the tray.
        // Secondary workspace windows must be allowed to close normally.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                    info!("window hidden to tray");
                }
            }
        })
        // Handle our custom "quit" menu item (Cmd-Q on macOS)
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "custom-quit" {
                confirm_quit(app);
            }
        })
        .setup(move |app| {
            let profile_state = profiles::ProfileState::load(app.handle(), &local_url)
                .map_err(anyhow::Error::msg)?;
            app.manage(profile_state);

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

            // Desktop owns the default local control-plane instance.
            let data_dir = local_instance_root();
            std::fs::create_dir_all(&data_dir).ok();
            let instance = data_dir.to_string_lossy().to_string();

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
            cmd.args(["up", "--instance", &instance])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            if std::env::var_os("XPRESSCLAW_PORT").is_some() {
                cmd.args(["--port", &port.to_string()]);
            }

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

            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(e) => {
                    warn!(
                        error = %e,
                        path = %sidecar_path.display(),
                        "failed to spawn sidecar — the app will start but the server won't be running"
                    );
                    tauri::async_runtime::spawn(show_preferred_startup(
                        app.handle().clone(),
                        local_url.clone(),
                        false,
                    ));
                    tray::setup_tray(app, &local_url)?;
                    return Ok(());
                }
            };

            if let Some(stdout) = child.stdout.take() {
                let token_handle = app.handle().clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    use zeroize::{Zeroize, Zeroizing};
                    for mut line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                        if let Some(token) = line.strip_prefix(STARTUP_TOKEN_PREFIX) {
                            let token = Zeroizing::new(token.to_string());
                            if let Err(error) = token_handle
                                .state::<profiles::ProfileState>()
                                .remember_local_startup_token(token.clone())
                            {
                                warn!(%error, "could not store the local startup token in the OS keychain");
                            }
                        }
                        line.zeroize();
                        // Drain every line without forwarding it: the token
                        // must never enter Desktop logs or diagnostics.
                    }
                });
            }

            info!(pid = child.id(), "sidecar spawned");

            let state = app.state::<SidecarState>();
            *state.0.lock().unwrap() = Some(child);

            // Resolve a selected remote profile before waiting on the local
            // sidecar. Remote operation must remain usable when the optional
            // automatic local instance cannot bind or start.
            tauri::async_runtime::spawn(show_preferred_startup(
                app.handle().clone(),
                local_url.clone(),
                true,
            ));

            tray::setup_tray(app, &local_url)?;
            info!(port, "xpressclaw desktop app started");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_health,
            commands::get_server_port,
            commands::get_status,
            commands::open_browser,
            profiles::list_instance_profiles,
            profiles::save_instance_profile,
            profiles::select_instance_profile,
            profiles::delete_instance_profile,
            profiles::get_active_instance_profile,
            profiles::login_active_profile,
            profiles::trust_local_instance_replacement,
            profiles::store_active_profile_credential,
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

async fn show_preferred_startup(
    handle: tauri::AppHandle,
    local_url: String,
    sidecar_spawned: bool,
) {
    let profile_state = handle.state::<profiles::ProfileState>();
    let url = profiles::preferred_startup_url(&profile_state).await;
    let local_profile = profile_state.active_is_local().unwrap_or(true);
    let server_ready = if should_wait_for_local_sidecar(local_profile, sidecar_spawned) {
        wait_for_server(&local_url).await
    } else {
        false
    };

    if server_ready {
        info!("server is ready");
        if let Err(error) = enable_image_paste_capability(&handle, &local_url) {
            warn!(%error, "failed to enable image paste capability");
        }
        if let Err(error) = enable_workspace_window_capability(&handle, &local_url) {
            warn!(%error, "failed to enable workspace window capability");
        }
    }
    // A remote profile is independent of local listener health. For the local
    // profile, do not grant native capabilities to an unverified listener.
    if !local_profile || server_ready {
        if let Err(error) = enable_profile_capabilities(&handle, &url, local_profile) {
            warn!(%error, "failed to enable selected profile capabilities");
        }
    }
    if let Some(window) = handle.get_webview_window("main") {
        if let Ok(url) = url.parse() {
            let _ = window.navigate(url);
        }
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn should_wait_for_local_sidecar(local_profile: bool, sidecar_spawned: bool) -> bool {
    local_profile && sidecar_spawned
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
///
/// Sends SIGTERM to the sidecar so it can run its graceful shutdown
/// (stop Docker containers, flush state). Falls back to SIGKILL if
/// the process doesn't exit within 10 seconds.
pub fn shutdown_sidecar(app: &tauri::AppHandle) {
    let mut child = app.state::<SidecarState>().0.lock().unwrap().take();
    let Some(ref mut child) = child else {
        return;
    };

    let pid = child.id();
    info!(pid, "sending SIGTERM to sidecar");

    // Send SIGTERM so the server's graceful shutdown runs
    // (stops Docker containers, cancels background tasks).
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        // Windows: no SIGTERM equivalent, just kill
        let _ = child.kill();
    }

    // Wait up to 15s for graceful exit, then force kill
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                info!(pid, ?status, "sidecar exited");
                return;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    warn!(pid, "sidecar did not exit in time — sending SIGKILL");
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => {
                warn!(pid, error = %e, "error waiting for sidecar");
                let _ = child.kill();
                return;
            }
        }
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

fn local_origins(url: &str) -> Vec<String> {
    let mut origins = vec![format!("{url}/*")];
    let port = reqwest::Url::parse(url).ok().and_then(|url| url.port());
    if cfg!(debug_assertions) && port != Some(DEV_FRONTEND_PORT) {
        origins.push(format!("http://localhost:{DEV_FRONTEND_PORT}/*"));
    }
    origins
}

fn enable_image_paste_capability(app: &tauri::AppHandle, url: &str) -> tauri::Result<()> {
    let mut capability = tauri::ipc::CapabilityBuilder::new("localhost-image-paste")
        .local(false)
        .window("main")
        .window("workspace-*")
        .permission("clipboard-manager:allow-read-image")
        .permission("core:image:allow-rgba")
        .permission("core:image:allow-size")
        .permission("core:resources:allow-close");
    for origin in local_origins(url) {
        capability = capability.remote(origin);
    }
    app.add_capability(capability)
}

fn enable_workspace_window_capability(app: &tauri::AppHandle, url: &str) -> tauri::Result<()> {
    let mut capability = tauri::ipc::CapabilityBuilder::new("localhost-workspace-windows")
        .local(false)
        .window("main")
        .window("workspace-*")
        .permission("core:webview:allow-create-webview-window")
        .permission("core:window:allow-create")
        .permission("core:window:allow-set-focus");
    for origin in local_origins(url) {
        capability = capability.remote(origin);
    }
    app.add_capability(capability)
}

/// Grant the selected profile's exact origin only its required Desktop
/// capabilities. Local content retains the legacy status/browser commands;
/// remote content receives only profile/login commands. No capability uses a
/// wildcard host or port.
pub(crate) fn enable_profile_capabilities(
    app: &tauri::AppHandle,
    url: &str,
    local_profile: bool,
) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("Selected profile is not an HTTP(S) origin".to_string());
    }
    let origin = parsed.origin().ascii_serialization();
    let mut capability = tauri::ipc::CapabilityBuilder::new(format!(
        "instance-profile-{}",
        uuid::Uuid::new_v4().simple()
    ))
    .local(false)
    .window("main")
    .window("workspace-*")
    .remote(format!("{origin}/*"));
    for permission in profile_capability_permissions(local_profile) {
        capability = capability.permission(permission);
    }
    app.add_capability(capability)
        .map_err(|error| error.to_string())
}

fn profile_capability_permissions(local_profile: bool) -> Vec<&'static str> {
    if !local_profile {
        // Runtime capabilities cannot be revoked after an origin is replaced
        // or deselected. Remote pages therefore receive only custom commands,
        // whose handlers revalidate the selected origin and pinned instance
        // identity on every call. In particular, never grant a remote origin
        // direct clipboard or window-plugin access.
        return vec!["desktop-profile-commands"];
    }

    vec![
        "clipboard-manager:allow-read-image",
        "core:image:allow-rgba",
        "core:image:allow-size",
        "core:resources:allow-close",
        "core:webview:allow-create-webview-window",
        "core:window:allow-create",
        "core:window:allow-set-focus",
        "desktop-local-commands",
    ]
}

fn is_xpressclaw_health(body: &serde_json::Value) -> bool {
    body.get("status").and_then(serde_json::Value::as_str) == Some("ok")
        && body.get("name").and_then(serde_json::Value::as_str) == Some("xpressclaw")
}

async fn wait_for_server(base_url: &str) -> bool {
    let url = format!("{base_url}/api/health");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build local sidecar health client");

    for i in 0..120 {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if resp
                    .json::<serde_json::Value>()
                    .await
                    .is_ok_and(|body| is_xpressclaw_health(&body))
                {
                    return true;
                }
            }
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if i % 20 == 19 {
            info!("waiting for server to start...");
        }
    }
    warn!("xpressclaw server did not become ready within 60 seconds");
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn local_origins_are_exact_and_port_scoped() {
        let origins = local_origins("http://127.0.0.1:19435");
        assert!(origins.contains(&"http://127.0.0.1:19435/*".to_string()));
        assert!(!origins.iter().any(|origin| origin.contains(":*")));
    }

    #[test]
    fn remote_profile_capabilities_expose_only_identity_checked_commands() {
        assert_eq!(
            profile_capability_permissions(false),
            vec!["desktop-profile-commands"]
        );
        let local = profile_capability_permissions(true);
        assert!(local.contains(&"clipboard-manager:allow-read-image"));
        assert!(local.contains(&"core:webview:allow-create-webview-window"));
        assert!(local.contains(&"desktop-local-commands"));
    }

    #[test]
    fn remote_startup_never_waits_for_the_local_sidecar() {
        assert!(!should_wait_for_local_sidecar(false, true));
        assert!(!should_wait_for_local_sidecar(false, false));
        assert!(should_wait_for_local_sidecar(true, true));
        assert!(!should_wait_for_local_sidecar(true, false));
    }

    #[test]
    fn listener_urls_turn_wildcards_into_reachable_local_addresses() {
        assert_eq!(
            listener_client_url("0.0.0.0".parse().unwrap(), 8935),
            "http://localhost:8935"
        );
        assert_eq!(
            listener_client_url("::".parse().unwrap(), 9443),
            "http://[::1]:9443"
        );
        assert_eq!(
            listener_client_url("100.64.0.8".parse().unwrap(), 9000),
            "http://100.64.0.8:9000"
        );
    }

    #[test]
    fn health_check_requires_xpressclaw_identity() {
        assert!(is_xpressclaw_health(&json!({
            "status": "ok",
            "name": "xpressclaw"
        })));
        assert!(!is_xpressclaw_health(&json!({ "status": "ok" })));
        assert!(!is_xpressclaw_health(&json!({
            "status": "ok",
            "name": "another-local-service"
        })));
    }
}
