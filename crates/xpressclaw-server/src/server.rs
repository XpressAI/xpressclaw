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
    // Extract built-in skills to the data directory
    if let Some(data_dir) = state.config_path.parent() {
        crate::skills::extract_skills(data_dir);
    }

    // Start host-side MCP servers from config.
    // These are available to harnesses via /v1/tools.
    let config = state.config();
    if !config.mcp_servers.is_empty() {
        info!(
            count = config.mcp_servers.len(),
            "starting MCP tool servers"
        );
        state.mcp_manager.start_servers(&config.mcp_servers).await;
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

    let app = create_router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!("xpressclaw server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
