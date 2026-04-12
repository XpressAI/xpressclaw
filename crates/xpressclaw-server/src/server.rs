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

    // Background processor scanner: checks for unprocessed messages
    // that were injected by the task dispatcher (or connectors) and
    // spawns processors for them. Runs every 5 seconds.
    {
        let scan_state = state.clone();
        let scan_shutdown = shutdown.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = async {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        scan_for_unprocessed(&scan_state).await;
                    }
                } => {}
                _ = scan_shutdown.cancelled() => { info!("processor scanner stopped"); }
            }
        });
    }

    // Start the desired-state reconciler (ADR-018).
    let reconciler_db = state.db.clone();
    let reconciler_config = state.config.clone();
    let reconciler_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::agents::reconciler::start(reconciler_db, reconciler_config, port) => {}
            _ = reconciler_shutdown.cancelled() => { info!("reconciler stopped"); }
        }
    });

    // Start the Wanix headless server as a child process.
    // Provides the agent filesystem environment at localhost:9100.
    let wanix_shutdown = shutdown.clone();
    let wanix_child = {
        // Find the wanix-server directory relative to the executable or cwd
        let wanix_script = find_wanix_server();
        if let Some(script) = wanix_script {
            info!(path = %script.display(), "starting Wanix headless server");
            match std::process::Command::new("node")
                .arg(&script)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
            {
                Ok(child) => {
                    let pid = child.id();
                    info!(pid, "Wanix server started");
                    Some(child)
                }
                Err(e) => {
                    warn!(error = %e, "failed to start Wanix server — filesystem tools will be unavailable");
                    None
                }
            }
        } else {
            warn!("wanix-server/index.mjs not found — filesystem tools will be unavailable");
            None
        }
    };

    // Clean up Wanix on shutdown
    let wanix_child_arc = std::sync::Arc::new(std::sync::Mutex::new(wanix_child));
    let wanix_cleanup = wanix_child_arc.clone();
    tokio::spawn(async move {
        wanix_shutdown.cancelled().await;
        if let Some(ref mut child) = *wanix_cleanup.lock().unwrap() {
            info!("stopping Wanix server");
            let _ = child.kill();
            let _ = child.wait();
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

    info!("shutdown complete");

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

/// Scan all conversations for unprocessed messages and spawn processors.
///
/// Only picks up messages that have been sitting unprocessed for at least
/// 10 seconds — this avoids racing with the HTTP handler which spawns
/// processors immediately. The scanner catches messages injected by the
/// task dispatcher, connectors, or other non-HTTP sources.
async fn scan_for_unprocessed(state: &AppState) {
    use xpressclaw_core::conversations::ConversationManager;

    let mgr = ConversationManager::new(state.db.clone());
    let convs = match mgr.list(100) {
        Ok(c) => c,
        Err(_) => return,
    };

    for conv in convs {
        // Skip if already processing or no unprocessed messages
        if mgr.is_processing(&conv.id) || !mgr.has_unprocessed(&conv.id) {
            continue;
        }

        // Only process if the unprocessed message has been sitting for > 10s.
        // This gives the HTTP handler time to spawn its own processor first.
        let stale = mgr
            .oldest_unprocessed_age(&conv.id)
            .map(|age| age > 10)
            .unwrap_or(false);
        if !stale {
            continue;
        }

        let Some(llm_router) = state.llm_router() else {
            continue;
        };

        let config = state.config();
        let agent_skills_map = config
            .agents
            .iter()
            .map(|a| (a.name.clone(), a.full_system_prompt()))
            .collect();

        info!(conv_id = conv.id, "scanner: spawning processor for stale unprocessed message");

        xpressclaw_core::conversations::processor::spawn(
            conv.id.clone(),
            xpressclaw_core::conversations::processor::ProcessorContext {
                db: state.db.clone(),
                config,
                llm_router,
                event_bus: state.event_bus.clone(),
                rate_limiter: state.rate_limiter(),
                agent_roles: agent_skills_map,
            },
        );
    }
}

/// Find the wanix-server/index.mjs script.
/// Checks: next to the executable, in the cwd, and in the source tree.
fn find_wanix_server() -> Option<std::path::PathBuf> {
    let candidates = [
        // Next to the executable (installed/packaged)
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("wanix-server").join("index.mjs"))),
        // Current working directory
        Some(std::path::PathBuf::from("wanix-server/index.mjs")),
        // Relative to the source tree (dev mode)
        std::env::current_exe()
            .ok()
            .and_then(|p| {
                // target/release/xpressclaw → project root
                p.ancestors().nth(3).map(|root| root.join("wanix-server").join("index.mjs"))
            }),
    ];

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}
