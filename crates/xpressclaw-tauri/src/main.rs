// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod tray;

use std::sync::Arc;

use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use xpressclaw_core::agents::registry::{AgentRegistry, RegisterAgent};
use xpressclaw_core::config::{self, Config};
use xpressclaw_core::db::Database;
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_core::llm::anthropic::AnthropicProvider;
use xpressclaw_core::llm::local::LocalProvider;
use xpressclaw_core::llm::openai::OpenAiProvider;
use xpressclaw_core::llm::router::LlmRouter;
use xpressclaw_server::server;
use xpressclaw_server::state::AppState;

const DEFAULT_PORT: u16 = 8935;

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

    let mut builder = tauri::Builder::default().plugin(tauri_plugin_shell::init());

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
        .setup(move |app| {
            // Start the Axum server in the background
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = start_server(port).await {
                    error!("server failed to start: {e}");
                    // Show error in tray tooltip if possible
                    if let Some(tray) = handle.tray_by_id("main-tray") {
                        let _ = tray.set_tooltip(Some(&format!("xpressclaw - Error: {e}")));
                    }
                }
            });

            // Set up system tray menu
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
        .run(tauri::generate_context!())
        .expect("error running xpressclaw desktop app");
}

async fn start_server(port: u16) -> anyhow::Result<()> {
    let state = build_state(port).await?;
    info!(port, "starting embedded server");
    server::serve(state, port).await
}

async fn build_state(port: u16) -> anyhow::Result<AppState> {
    let mut config = Config::load_default()?;
    config::env_overrides(&mut config);

    info!(agents = config.agents.len(), "loaded configuration");

    // Docker check — warn but don't block desktop app startup
    match DockerManager::connect().await {
        Ok(_) => info!("container runtime available"),
        Err(e) => {
            warn!(
                "Docker/Podman not available: {e}. \
                 Agent containers will not work until a container runtime is installed."
            );
        }
    }

    // Open database
    let db_path = config.system.data_dir.join("xpressclaw.db");
    let db = Arc::new(Database::open(&db_path)?);
    info!(path = %db_path.display(), "database ready");

    // Register agents from config
    let registry = AgentRegistry::new(db.clone());
    for agent_config in &config.agents {
        let mut agent_json = serde_json::Map::new();
        if !agent_config.role.is_empty() {
            agent_json.insert(
                "role".into(),
                serde_json::Value::String(agent_config.role.clone()),
            );
        }
        if let Some(ref model) = agent_config.model {
            agent_json.insert("model".into(), serde_json::Value::String(model.clone()));
        }

        match registry.register(&RegisterAgent {
            name: agent_config.name.clone(),
            backend: agent_config.backend.clone(),
            config: serde_json::Value::Object(agent_json),
        }) {
            Ok(record) => {
                info!(
                    name = record.name,
                    backend = record.backend,
                    "registered agent"
                )
            }
            Err(e) => warn!(name = agent_config.name, error = %e, "failed to register agent"),
        }
    }

    // Build LLM router
    let config = Arc::new(config);
    let llm_router = {
        let mut router = LlmRouter::new(&config.llm);

        if let Some(ref key) = config.llm.openai_api_key {
            let provider =
                OpenAiProvider::new(Some(key.clone()), config.llm.openai_base_url.clone());
            router.register_provider("openai", Arc::new(provider));
        }

        if let Some(ref key) = config.llm.anthropic_api_key {
            let provider = AnthropicProvider::new(key.clone());
            router.register_provider("anthropic", Arc::new(provider));
        }

        if let Some(ref model) = config.llm.local_model {
            let provider = LocalProvider::ollama(model.clone());
            router.register_provider("local", Arc::new(provider));
        }

        router
    };

    let _ = port;

    Ok(AppState {
        config,
        db,
        llm_router: Some(Arc::new(llm_router)),
    })
}
