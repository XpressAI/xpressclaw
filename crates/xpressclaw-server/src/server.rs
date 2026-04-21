use std::net::SocketAddr;

use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

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

    // ADR-023: install the agent harness.
    //
    // Default: `EchoHarness` — in-process, binds a per-agent TCP
    // listener and serves OpenAI-compatible chat completions by
    // forwarding through the LlmRouter. Works out of the box; no
    // external images required.
    //
    // Override: `XPRESSCLAW_HARNESS=pi` flips to `PiHarness` on c2w +
    // wasmtime. Use this when you have a WASM harness image to test
    // against (local podman registry during dev, GHCR in production).
    let harness_backend = std::env::var("XPRESSCLAW_HARNESS").unwrap_or_else(|_| "echo".into());
    if let Some(data_dir) = state.config_path.parent() {
        for dir in [data_dir.join("harness-cache"), data_dir.join("workspaces")] {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                warn!(path = %dir.display(), error = %e, "failed to create harness dir");
            }
        }
        match harness_backend.as_str() {
            "pi" => match xpressclaw_core::c2w::C2wRuntime::new() {
                Ok(runtime) => {
                    let harness_cache = data_dir.join("harness-cache");
                    let workspaces_root = data_dir.join("workspaces");
                    let c2w = std::sync::Arc::new(xpressclaw_core::harness::C2wHarness::new(
                        runtime,
                        harness_cache.clone(),
                    ));
                    let resolver = xpressclaw_core::harness::HarnessImageResolver::with_fallback(
                        harness_cache,
                    );
                    let pi =
                        xpressclaw_core::harness::PiHarness::new(c2w, resolver, workspaces_root);
                    state.set_harness(std::sync::Arc::new(pi));
                    info!("pi harness installed (c2w on wasmtime, ADR-023 task 10 phase 2)");
                }
                Err(e) => warn!(
                    error = %e,
                    "failed to initialise wasm runtime for pi harness — falling back to echo"
                ),
            },
            _ => {
                let echo = std::sync::Arc::new(crate::echo_harness::EchoHarness::new(
                    state.llm_router.clone(),
                    state.mcp_manager.clone(),
                ));
                state.set_harness(echo);
                info!(
                    "echo harness installed (ADR-023, default; set XPRESSCLAW_HARNESS=pi for c2w)"
                );
            }
        }
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

    // Start the xclaw shell-bridge listener (ADR-023 task 5).
    // Socket lives under the xpressclaw data dir so it moves with the
    // rest of runtime state. The pi harness (task 4) mounts this socket
    // into guest workspaces so agents can invoke `xclaw <verb>`.
    if let Some(data_dir) = state.config_path.parent() {
        let socket_path = crate::xclaw_bridge::default_socket_path(data_dir);
        match crate::xclaw_bridge::start(socket_path.clone(), state.clone()) {
            Ok(_handle) => info!(
                socket = %socket_path.display(),
                "xclaw bridge started"
            ),
            Err(e) => warn!(
                error = %e,
                "xclaw bridge failed to start; agents will not be able to call xclaw verbs"
            ),
        }
    }

    // Shutdown token: cancels all background tasks on Ctrl+C.
    let shutdown = tokio_util::sync::CancellationToken::new();

    // Start the task dispatcher background loop.
    let dispatcher_db = state.db.clone();
    let dispatcher_config = state.config();
    let dispatcher_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::tasks::dispatcher::start_dispatcher(dispatcher_db, dispatcher_config) => {}
            _ = dispatcher_shutdown.cancelled() => { info!("dispatcher stopped"); }
        }
    });

    // Start the cron schedule runner.
    let scheduler_db = state.db.clone();
    let scheduler_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::tasks::scheduler::start_schedule_runner(scheduler_db) => {}
            _ = scheduler_shutdown.cancelled() => { info!("scheduler stopped"); }
        }
    });

    // Start the desired-state reconciler (ADR-018, ADR-023 task 10).
    let reconciler_db = state.db.clone();
    let reconciler_config = state.config.clone();
    let reconciler_harness = state.harness().await;
    let reconciler_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::agents::reconciler::start(
                reconciler_db, reconciler_config, port, reconciler_harness,
            ) => {}
            _ = reconciler_shutdown.cancelled() => { info!("reconciler stopped"); }
        }
    });

    // Recover any running workflow instances after restart.
    {
        let engine = xpressclaw_core::workflows::engine::WorkflowEngine::new(state.db.clone());
        match engine.recover() {
            Ok(()) => info!("workflow engine recovery complete"),
            Err(e) => warn!(error = %e, "workflow engine recovery failed"),
        }
    }

    // Start connector runtime: launch all enabled connectors and route their events.
    let connector_db = state.db.clone();
    let connector_state = state.clone();
    let connector_shutdown = shutdown.clone();
    tokio::spawn(async move {
        use xpressclaw_core::connectors::registry::ConnectorRegistry;
        use xpressclaw_core::connectors::router;
        use xpressclaw_core::workflows::engine::WorkflowEngine;

        let mut registry = ConnectorRegistry::new(connector_db.clone());
        let mut event_rx = registry.take_event_receiver().unwrap();

        // Start all enabled connectors (telegram polling, file watchers, etc.)
        match registry.start_all().await {
            Ok(()) => info!("connector registry started"),
            Err(e) => warn!(error = %e, "some connectors failed to start"),
        }

        let engine = WorkflowEngine::new(connector_db.clone());

        // Event processing loop: route incoming connector events
        loop {
            tokio::select! {
                Some(event) = event_rx.recv() => {
                    // Route event: direct agent binding → conversation, or → workflow engine
                    if let Some((conv_id, _agent_id)) = router::route_event(&connector_db, &event) {
                        // Message was injected into a conversation — spawn the processor
                        let mgr = xpressclaw_core::conversations::ConversationManager::new(connector_db.clone());
                        if !mgr.is_processing(&conv_id) {
                            if let Some(llm_router) = connector_state.llm_router() {
                                let config = connector_state.config();
                                let agent_roles = config.agents.iter()
                                    .map(|a| (a.name.clone(), a.full_system_prompt()))
                                    .collect();
                                xpressclaw_core::conversations::processor::spawn(
                                    conv_id,
                                    xpressclaw_core::conversations::processor::ProcessorContext {
                                        db: connector_db.clone(),
                                        config,
                                        llm_router,
                                        event_bus: connector_state.event_bus.clone(),
                                        rate_limiter: connector_state.rate_limiter(),
                                        agent_roles,
                                        harness: connector_state.harness().await,
                                    },
                                );
                            }
                        }
                    }
                    // Also let the workflow engine check for matching triggers
                    match engine.process_events() {
                        Ok(n) if n > 0 => info!(count = n, "triggered workflow instances"),
                        Err(e) => warn!(error = %e, "workflow event processing failed"),
                        _ => {}
                    }
                }
                // Also poll periodically for events recorded via webhook API
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                    match engine.process_events() {
                        Ok(n) if n > 0 => info!(count = n, "processed connector events"),
                        Err(e) => warn!(error = %e, "workflow event processing failed"),
                        _ => {}
                    }
                }
                _ = connector_shutdown.cancelled() => {
                    info!("stopping connectors...");
                    let _ = registry.stop_all().await;
                    info!("connector runtime stopped");
                    break;
                }
            }
        }
    });

    let app = create_router(state.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    info!("xpressclaw server listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    // Cancel all background tasks immediately
    shutdown.cancel();

    // Graceful shutdown: stop agents via the harness (if one is wired
    // up). A second Ctrl+C during shutdown forces immediate exit.
    info!("shutting down — stopping agents (Ctrl+C again to force quit)");

    let shutdown_task = async {
        if let Some(harness) = state.harness().await {
            if let Err(e) = harness.stop_all().await {
                warn!(error = %e, "harness stop_all failed");
            } else {
                info!("all agents stopped");
            }
        } else {
            info!("no harness wired (ADR-023 spike); nothing to stop");
        }
    };

    tokio::select! {
        _ = shutdown_task => {}
        _ = tokio::signal::ctrl_c() => {
            info!("force quit — skipping container cleanup");
        }
        _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {
            warn!("shutdown timed out after 15s — skipping remaining containers");
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received SIGINT");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM");
            }
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
        info!("received shutdown signal");
    }
}
