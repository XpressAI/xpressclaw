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

    let config = state.config();

    // Shutdown token: cancels all background tasks on Ctrl+C.
    let shutdown = tokio_util::sync::CancellationToken::new();

    // Ensure every configured runtime context has a durable logical session.
    {
        let sessions = xpressclaw_core::sessions::SessionManager::new(state.db.clone());
        for agent in &config.agents {
            let title = agent.context_label();
            if let Err(error) = sessions.ensure(&agent.name, Some(&title)) {
                warn!(agent_id = agent.name, error = %error, "failed to initialize session");
            }
        }
    }

    // A process restart severs ownership of in-flight worker containers.
    // Remove any leftovers, restore their durable queue records, and let the
    // normal dispatcher retry them from a known state.
    let dispatcher_docker = state.docker().await;
    if let Some(docker) = dispatcher_docker.as_ref() {
        match docker.list().await {
            Ok(containers) => {
                for container in containers {
                    let is_attempt = container.agent_id.starts_with("attempt-");
                    let is_legacy_session = config
                        .agents
                        .iter()
                        .any(|agent| agent.name == container.agent_id);
                    if is_attempt || is_legacy_session {
                        let _ = docker.stop(&container.agent_id).await;
                    }
                }
            }
            Err(error) => {
                warn!(error = %error, "failed to inspect interrupted worker containers")
            }
        }
    }
    match xpressclaw_core::tasks::queue::TaskQueue::new(state.db.clone()).recover_in_progress() {
        Ok(count) if count > 0 => info!(count, "requeued interrupted work attempts"),
        Err(error) => warn!(error = %error, "failed to recover interrupted work attempts"),
        _ => {}
    }

    // Recover workflow bookkeeping before workers can claim recovered tasks.
    {
        let engine = xpressclaw_core::workflows::engine::WorkflowEngine::new(state.db.clone());
        match engine.recover() {
            Ok(()) => info!("workflow engine recovery complete"),
            Err(e) => warn!(error = %e, "workflow engine recovery failed"),
        }
    }

    // Consume tasks with short-lived ACP agent workers. The former harness
    // dispatcher and desired-state agent reconciler are intentionally not
    // started: runtime contexts are durable sessions, not long-running loops.
    let dispatcher_db = state.db.clone();
    let dispatcher_config = state.config.clone();
    let dispatcher_event_bus = state.event_bus.clone();
    let dispatcher_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::workers::native::start_dispatcher(
                dispatcher_db,
                dispatcher_config,
                dispatcher_docker,
                dispatcher_event_bus,
            ) => {}
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

    // Start connector runtime: launch all enabled connectors and route their events.
    let connector_db = state.db.clone();
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
                    if let Some((conv_id, agent_id)) = router::route_event(&connector_db, &event) {
                        // Direct connector messages are session events that queue
                        // native work, just like UI messages and schedules.
                        let sessions = xpressclaw_core::sessions::SessionManager::new(connector_db.clone());
                        let _ = sessions.ensure(&agent_id, Some(&agent_id));
                        let summary = event.payload.get("text")
                            .or_else(|| event.payload.get("message"))
                            .or_else(|| event.payload.get("content"))
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                            .unwrap_or_else(|| event.payload.to_string());
                        let source_id = format!("{}:{}", event.connector_id, event.channel_id);
                        let _ = sessions.append_event(
                            &agent_id,
                            xpressclaw_core::sessions::NewEvent {
                                attempt_id: None,
                                task_id: None,
                                source_type: "connector",
                                source_id: Some(&source_id),
                                event_type: &event.event_type,
                                summary: &summary,
                                payload: event.payload.clone(),
                            },
                        );
                        let board = xpressclaw_core::tasks::board::TaskBoard::new(connector_db.clone());
                        if let Ok(task) = board.create(&xpressclaw_core::tasks::board::CreateTask {
                            title: format!("{} message", event.connector_id),
                            description: Some(summary),
                            agent_id: Some(agent_id.clone()),
                            parent_task_id: None,
                            sop_id: None,
                            conversation_id: Some(conv_id),
                            priority: None,
                            context: Some(serde_json::json!({
                                "origin": "connector",
                                "kind": "interactive",
                                "source_id": source_id,
                                "connector_id": event.connector_id,
                                "channel_id": event.channel_id,
                            })),
                        }) {
                            let queue = xpressclaw_core::tasks::queue::TaskQueue::new(connector_db.clone());
                            if let Err(error) = queue.enqueue(&task.id, &agent_id) {
                                warn!(task_id = task.id, error = %error, "failed to queue connector work");
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

    // Graceful shutdown: stop containers with a timeout.
    // A second Ctrl+C during shutdown forces immediate exit.
    info!("shutting down — stopping containers (Ctrl+C again to force quit)");

    let shutdown_task = async {
        if let Ok(docker) = xpressclaw_core::docker::manager::DockerManager::connect().await {
            let _ = docker.stop_all().await;
            info!("all containers stopped");
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
