use std::net::SocketAddr;

use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::frontend;
use crate::routes;
use crate::state::AppState;

/// Create the main Axum router with all routes.
pub fn create_router(state: AppState) -> Router {
    Router::new()
        .nest("/api", routes::api_routes())
        .nest("/v1", routes::llm::routes())
        .nest("/v1/tools", routes::tools_proxy_routes())
        .nest("/apps", routes::app_proxy_routes())
        // Serve embedded SvelteKit frontend for all other paths
        .fallback(frontend::serve_frontend)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Start the HTTP server.
pub async fn serve(state: AppState, port: u16) -> anyhow::Result<()> {
    // Log frontend embed status (debug diagnostic)
    crate::frontend::log_frontend_status();

    // Extract built-in skills to the data directory
    if let Some(data_dir) = state.config_path.parent() {
        crate::skills::extract_skills(data_dir);
    }

    // Start host-side MCP servers in background.
    // Skip servers with container-only paths — those only run inside Docker.
    let config = state.config();
    let host_servers: std::collections::HashMap<String, _> = config
        .mcp_servers
        .iter()
        .filter(|(_, cfg)| {
            // Skip servers whose command or args reference container paths
            !cfg.args
                .iter()
                .any(|a| a.starts_with("/app/") || a.starts_with("/workspace"))
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if !host_servers.is_empty() {
        let mcp_mgr = state.mcp_manager.clone();
        info!(
            count = host_servers.len(),
            servers = ?host_servers.keys().collect::<Vec<_>>(),
            "starting host MCP tool servers in background"
        );
        tokio::spawn(async move {
            mcp_mgr.start_servers(&host_servers).await;
        });
    }

    // Start the task dispatcher background loop.
    let dispatcher_db = state.db.clone();
    let dispatcher_config = state.config();
    tokio::spawn(async move {
        xpressclaw_core::tasks::dispatcher::start_dispatcher(dispatcher_db, dispatcher_config)
            .await;
    });

    // Start the cron schedule runner.
    let scheduler_db = state.db.clone();
    tokio::spawn(async move {
        xpressclaw_core::tasks::scheduler::start_schedule_runner(scheduler_db).await;
    });

    // Start the desired-state reconciler (ADR-018).
    // Continuously converges agents, images, and tasks toward desired state.
    // Passes the RwLock config so the reconciler always sees the latest config
    // after user changes (add/remove agents, update API keys, etc.)
    let reconciler_db = state.db.clone();
    let reconciler_config = state.config.clone(); // Arc<RwLock<Arc<Config>>>
    tokio::spawn(async move {
        xpressclaw_core::agents::reconciler::start(reconciler_db, reconciler_config, port).await;
    });

    let app = create_router(state.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!("xpressclaw server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Graceful shutdown: stop all agent and app containers
    info!("shutting down — stopping containers");
    if let Ok(docker) = xpressclaw_core::docker::manager::DockerManager::connect().await {
        let registry = xpressclaw_core::agents::registry::AgentRegistry::new(state.db.clone());
        if let Ok(agents) = registry.list() {
            for agent in &agents {
                let _ = docker.stop(&agent.id).await;
            }
        }
        // Stop app containers
        let apps: Vec<String> = {
            let conn = state.db.conn();
            conn.prepare("SELECT id FROM apps WHERE status IN ('running', 'starting')")
                .and_then(|mut stmt| {
                    stmt.query_map([], |row| row.get::<_, String>(0))
                        .map(|rows| rows.filter_map(|r| r.ok()).collect())
                })
                .unwrap_or_default()
        };
        for app_id in &apps {
            let _ = docker.stop(&format!("app-{app_id}")).await;
        }
        info!("all containers stopped");
    }

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
    info!("received shutdown signal");
}
