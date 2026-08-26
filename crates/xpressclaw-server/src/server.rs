use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::auth::{cookie_value, InstanceAuth, CSRF_HEADER};
use crate::frontend;
use crate::routes;
use crate::state::AppState;

/// Create the main Axum router with all routes.
pub fn create_router(state: AppState) -> Router {
    let api = routes::public_api_routes(&state);
    Router::new()
        .nest("/api", api)
        // Serve embedded SvelteKit frontend for all other paths
        .fallback(frontend::serve_frontend)
        // Base64 encoding expands image messages beyond Axum's 2 MiB JSON default.
        // Message handlers enforce tighter decoded per-image and aggregate limits.
        .layer(DefaultBodyLimit::max(30 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn create_unprotected_router(state: AppState) -> Router {
    Router::new()
        .nest("/api", routes::api_routes())
        .fallback(frontend::serve_frontend)
        .layer(DefaultBodyLimit::max(30 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn create_internal_router(state: AppState, token: Arc<str>) -> Router {
    create_unprotected_router(state).layer(middleware::from_fn_with_state(
        token,
        require_internal_token,
    ))
}

/// Authenticate the complete public user-facing API boundary, including SSE
/// streams and WebSocket upgrade requests. The runner callback router never
/// uses this middleware and retains its independent capability.
pub(crate) async fn require_user_session(
    State(auth): State<Arc<InstanceAuth>>,
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    if !auth.enabled() {
        return Ok(next.run(request).await);
    }

    let Some(cookie) = cookie_value(request.headers()) else {
        return Err(routes::auth::unauthorized());
    };
    if auth.authenticate(cookie).is_none() {
        return Err(routes::auth::unauthorized());
    }

    if !routes::auth::is_safe_method(request.method()) {
        let Some(csrf) = request
            .headers()
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
        else {
            return Err(routes::auth::error_response(
                StatusCode::FORBIDDEN,
                "CSRF token required",
            ));
        };
        if !auth.verify_csrf(cookie, csrf) {
            return Err(routes::auth::error_response(
                StatusCode::FORBIDDEN,
                "CSRF token is invalid or expired",
            ));
        }
    }

    Ok(next.run(request).await)
}

async fn require_internal_token(
    State(token): State<Arc<str>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let supplied = request
        .headers()
        .get("x-xpressclaw-internal-token")
        .and_then(|value| value.to_str().ok());
    // Git's smart-HTTP client cannot attach the callback header. This one
    // narrow route authenticates independently with a per-Agent revocable
    // Basic capability before proxying to GitBucket; every other callback
    // route still requires the process-scoped internal token.
    let collaboration_git_proxy = request
        .uri()
        .path()
        .starts_with("/api/settings/collaboration/agent/git/");
    if supplied != Some(token.as_ref()) && !collaboration_git_proxy {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(request).await)
}

/// Start the HTTP server on the safe local default.
pub async fn serve(state: AppState, port: u16) -> anyhow::Result<()> {
    serve_on(state, IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port).await
}

/// Start the HTTP server on an explicitly selected client address.
pub async fn serve_on(state: AppState, bind: IpAddr, port: u16) -> anyhow::Result<()> {
    serve_on_with_bound_callback(state, bind, port, || Ok(())).await
}

/// Start the HTTP server and invoke `on_bound` only after both public and
/// runner-callback listeners are owned by this process. Launchers use this
/// boundary to publish per-start credentials without announcing a token from
/// a process that will subsequently lose a port race.
pub async fn serve_on_with_bound_callback(
    state: AppState,
    bind: IpAddr,
    port: u16,
    on_bound: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Log frontend embed status (debug diagnostic)
    crate::frontend::log_frontend_status();

    // Browsers use the requested address. Runner containers use a separate,
    // ephemeral callback listener because Docker/Podman's host gateway cannot
    // reach a host service bound only to loopback on Linux. Every callback
    // request requires a per-process capability that is injected into the
    // bundled MCP processes and never exposed to the browser.
    let public_addr = SocketAddr::new(bind, port);
    let public_listener = tokio::net::TcpListener::bind(public_addr).await?;
    let internal_listener = tokio::net::TcpListener::bind("0.0.0.0:0").await?;
    let internal_port = internal_listener.local_addr()?.port();
    let internal_token: Arc<str> = Arc::from(uuid::Uuid::new_v4().simple().to_string());
    on_bound()?;

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

    // Older builds treated ACP plan rows as durable child tasks. Reconcile
    // only parents whose latest attempt already succeeded and which have no
    // queued or active work, then let workflow recovery advance any repaired
    // step normally.
    match xpressclaw_core::tasks::board::TaskBoard::new(state.db.clone())
        .reconcile_finished_reported_plans()
    {
        Ok(reconciliation)
            if reconciliation.deferred_items > 0 || !reconciliation.completed_tasks.is_empty() =>
        {
            info!(
                deferred_plan_items = reconciliation.deferred_items,
                completed_tasks = reconciliation.completed_tasks.len(),
                "reconciled completed work stranded by legacy ACP plans"
            )
        }
        Err(error) => warn!(error = %error, "failed to reconcile legacy ACP plans"),
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
    let dispatcher_conversation_processes = state.conversation_processes.clone();
    let dispatcher_runtime_lifecycle = state.native_runtime_lifecycle.clone();
    let dispatcher_control_token = internal_token.clone();
    let dispatcher_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = xpressclaw_core::workers::native::start_dispatcher(
                dispatcher_db,
                dispatcher_config,
                dispatcher_docker,
                xpressclaw_core::workers::native::NativeDispatcherServices {
                    event_bus: dispatcher_event_bus,
                    elicitation_broker: dispatcher_elicitations,
                    turn_controls: dispatcher_turn_controls,
                    conversation_processes: dispatcher_conversation_processes,
                    runtime_lifecycle: dispatcher_runtime_lifecycle,
                    control_plane_token: dispatcher_control_token,
                },
                internal_port,
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

    let public_app = create_router(state.clone());
    let internal_app = create_internal_router(state.clone(), internal_token);

    info!("xpressclaw server listening on http://{public_addr}");
    info!(port = internal_port, "runner callback listener ready");

    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        shutdown_signal().await;
        signal_shutdown.cancel();
    });
    let public_shutdown = shutdown.clone();
    let internal_shutdown = shutdown.clone();
    let servers = tokio::try_join!(
        axum::serve(
            public_listener,
            public_app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(public_shutdown.cancelled_owned()),
        axum::serve(internal_listener, internal_app)
            .with_graceful_shutdown(internal_shutdown.cancelled_owned()),
    );
    shutdown.cancel();
    servers?;

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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::{header, Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;
    use zeroize::Zeroizing;

    use super::*;

    fn state() -> AppState {
        let mut config = Config::default();
        config.system.data_dir = std::env::temp_dir().join(format!(
            "xpressclaw-empty-callback-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            "test.yaml".into(),
            true,
        )
    }

    fn authenticated_state() -> (AppState, Zeroizing<String>) {
        let root = tempfile::tempdir().unwrap().keep();
        let mut config = Config::default();
        config.system.data_dir = root.clone();
        config.system.workspace_dir = root.join("workspaces");
        config.instance.authentication_enabled = true;
        let effective = config.instance.clone();
        let db = Arc::new(Database::open_memory().unwrap());
        let token = Zeroizing::new("test-startup-token-with-enough-entropy".to_string());
        let state = AppState::new_with_instance(
            Arc::new(config),
            db,
            None,
            root.join("xpressclaw.yaml"),
            true,
            effective,
            Some(token.clone()),
        )
        .unwrap();
        (state, token)
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn login_session(app: &Router, token: &str) -> (String, String) {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "credential": token }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let body = json_body(response).await;
        (cookie, body["csrf_token"].as_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn runner_callback_listener_requires_its_ephemeral_capability() {
        let token: Arc<str> = Arc::from("internal-secret");
        let app = create_internal_router(state(), token);

        let unauthorized = app
            .clone()
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let independently_authenticated_git_proxy = app
            .clone()
            .oneshot(
                Request::get(
                    "/api/settings/collaboration/agent/git/xpressclaw-agent/demo/info/refs?service=git-receive-pack",
                )
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            independently_authenticated_git_proxy.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let authorized = app
            .oneshot(
                Request::get("/api/health")
                    .header("x-xpressclaw-internal-token", "internal-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn public_auth_gates_http_stream_file_and_websocket_route_families() {
        let (state, token) = authenticated_state();
        let app =
            create_router(state).layer(MockConnectInfo(SocketAddr::from(([192, 0, 2, 9], 43100))));

        for path in [
            "/api/agents",
            "/api/dashboard/stream?range=1h",
            "/api/conversations/example/attachments/example",
            "/api/tasks/example/messages/1/attachments/example",
            "/api/settings/collaboration/agent/git/example/repository/info/refs?service=git-receive-pack",
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
        let websocket = app
            .clone()
            .oneshot(
                Request::get("/api/workspaces/example/terminal")
                    .header(header::CONNECTION, "upgrade")
                    .header(header::UPGRADE, "websocket")
                    .header("sec-websocket-version", "13")
                    .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(websocket.status(), StatusCode::UNAUTHORIZED);

        let health = app
            .clone()
            .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);
        let bootstrap = app
            .clone()
            .oneshot(
                Request::get("/api/auth/bootstrap")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::OK);

        let (cookie, _) = login_session(&app, &token).await;
        let agents = app
            .oneshot(
                Request::get("/api/agents")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(agents.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn authenticated_mutations_require_matching_csrf_and_logout_revokes_session() {
        let (state, token) = authenticated_state();
        let app =
            create_router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 43101))));
        let (cookie, csrf) = login_session(&app, &token).await;
        let body = serde_json::json!({ "name": "Protected project" }).to_string();

        let missing = app
            .clone()
            .oneshot(
                Request::post("/api/projects")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::FORBIDDEN);

        let accepted = app
            .clone()
            .oneshot(
                Request::post("/api/projects")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(crate::auth::CSRF_HEADER, &csrf)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::CREATED);

        let logout = app
            .clone()
            .oneshot(
                Request::post("/api/auth/logout")
                    .header(header::COOKIE, &cookie)
                    .header(crate::auth::CSRF_HEADER, &csrf)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        let stale = app
            .oneshot(
                Request::get("/api/agents")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn browser_auth_never_replaces_the_runner_callback_capability() {
        let (state, _) = authenticated_state();
        let internal = create_internal_router(state, Arc::from("runner-capability"));
        let authorized = internal
            .oneshot(
                Request::get("/api/agents")
                    .header("x-xpressclaw-internal-token", "runner-capability")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authorized.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn relogin_prevents_session_fixation_and_https_origin_sets_secure_cookie() {
        let (state, token) = authenticated_state();
        let app =
            create_router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 43102))));
        let (old_cookie, _) = login_session(&app, &token).await;
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/auth/login")
                    .header(header::COOKIE, &old_cookie)
                    .header(header::ORIGIN, "https://xpressclaw.example")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "credential": token.as_str() }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let set_cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("; Secure"));
        let new_cookie = set_cookie.split(';').next().unwrap().to_string();
        assert_ne!(new_cookie, old_cookie);

        let stale = app
            .clone()
            .oneshot(
                Request::get("/api/agents")
                    .header(header::COOKIE, old_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);
        let current = app
            .oneshot(
                Request::get("/api/agents")
                    .header(header::COOKIE, new_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(current.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn authentication_payloads_do_not_inherit_the_attachment_body_limit() {
        let (state, _) = authenticated_state();
        let app =
            create_router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 43103))));
        let response = app
            .oneshot(
                Request::post("/api/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "credential": "x".repeat(9 * 1024) }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
