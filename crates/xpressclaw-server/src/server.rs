use std::net::SocketAddr;

use axum::extract::DefaultBodyLimit;
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
        // Base64 encoding expands image messages beyond Axum's 2 MiB JSON default.
        // Message handlers enforce tighter decoded per-image and aggregate limits.
        .layer(DefaultBodyLimit::max(30 * 1024 * 1024))
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

    // A process restart severs ownership of in-flight ACP processes. Stop
    // retained project containers without deleting their writable layers,
    // remove obsolete containers owned by this installation, then retry
    // durable work. Foreign and unlabelled containers are not enumerated.
    let dispatcher_docker = state.docker().await;
    if let Some(docker) = dispatcher_docker.as_ref() {
        match docker.list().await {
            Ok(containers) => {
                for container in containers {
                    let is_attempt = container.agent_id.starts_with("attempt-");
                    let is_configured_project = config
                        .agents
                        .iter()
                        .any(|agent| agent.name == container.agent_id);
                    let is_project_container =
                        docker.is_project_container(&container.agent_id).await;
                    if is_project_container && is_configured_project {
                        let _ = docker.stop_preserving(&container.agent_id).await;
                    } else if is_attempt || is_project_container || is_configured_project {
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

    // Consume tasks with ACP agent processes inside retained project
    // containers. The former harness dispatcher and desired-state agent
    // reconciler remain disabled: runtime contexts are durable sessions, not
    // long-running autonomous loops.
    let dispatcher_db = state.db.clone();
    let dispatcher_config = state.config.clone();
    let dispatcher_event_bus = state.event_bus.clone();
    let dispatcher_elicitations = state.elicitations.clone();
    let dispatcher_turn_controls = state.turn_controls.clone();
    let dispatcher_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::workers::native::start_dispatcher(
                dispatcher_db,
                dispatcher_config,
                dispatcher_docker,
                dispatcher_event_bus,
                dispatcher_elicitations,
                dispatcher_turn_controls,
                port,
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

    // Start automatic cron triggers for multi-step workflows.
    let workflow_scheduler_db = state.db.clone();
    let workflow_scheduler_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::workflows::scheduler::start_schedule_runner(workflow_scheduler_db) => {}
            _ = workflow_scheduler_shutdown.cancelled() => { info!("workflow scheduler stopped"); }
        }
    });

    // Resume durable workflow event waits (for example PR review activity).
    // The bound agent selects the project workspace and scoped GitHub access;
    // no worker container remains alive while the workflow is sleeping.
    let workflow_wait_db = state.db.clone();
    let workflow_wait_config = state.config.clone();
    let workflow_wait_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::workflows::waits::start_wait_runner(workflow_wait_db, workflow_wait_config) => {}
            _ = workflow_wait_shutdown.cancelled() => { info!("workflow wait runner stopped"); }
        }
    });

    // Keep ordinary PR-producing tasks open through human/automated review.
    // New feedback resumes the same task conversation; approval or merge
    // releases the agent's queue lane and completes the task.
    let github_review_db = state.db.clone();
    let github_review_config = state.config.clone();
    let github_review_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::workers::github_review::start_review_runner(
                github_review_db,
                github_review_config,
            ) => {}
            _ = github_review_shutdown.cancelled() => { info!("GitHub task review runner stopped"); }
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
        if let Some(docker) = state.docker().await {
            let _ = docker.stop_all_for_shutdown().await;
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
