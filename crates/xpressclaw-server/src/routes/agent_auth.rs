//! User-facing Agent sign-in. Login output and input are never persisted in task history.
use crate::state::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use xpressclaw_core::{
    agents::registry::AgentRegistry,
    config::AgentConfig,
    docker::manager::DockerManager,
    sessions::{NewEvent, SessionManager},
    workers::{
        acp::{
            authentication::AuthenticationBroker, AcpAuthentication, AuthMethod,
            CreateElicitationResponse,
        },
        native::{authentication, resolve_runner_kind},
    },
};

type ApiError = (StatusCode, Json<Value>);
fn failure(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": error.to_string()})),
    )
}

#[derive(Serialize)]
struct Method {
    id: String,
    name: String,
    description: Option<String>,
    kind: &'static str,
    api_key: bool,
}

fn methods(auth: &AcpAuthentication) -> Vec<Method> {
    auth.methods
        .iter()
        .filter_map(|method| {
            let kind = match method {
                AuthMethod::Terminal(_) => "terminal",
                AuthMethod::Agent(_) => "agent",
                _ => return None,
            };
            Some(Method {
                id: method.id().to_string(),
                name: method.name().to_owned(),
                description: method.description().map(str::to_owned),
                kind,
                api_key: method
                    .meta()
                    .is_some_and(|meta| meta.contains_key("api-key")),
            })
        })
        .collect()
}

fn login_command(
    prepared: &authentication::PreparedAuthentication,
    method_id: &str,
) -> Result<(Vec<String>, Vec<String>), ApiError> {
    let auth = prepared.process.authentication();
    if let Some(AuthMethod::Terminal(method)) = auth
        .methods
        .iter()
        .find(|method| method.id().to_string() == method_id)
    {
        let mut command = prepared
            .spec
            .cmd
            .clone()
            .ok_or_else(|| failure("The Agent has no launch command"))?;
        command.extend(method.args.clone());
        let mut environment = vec![];
        environment.extend(
            method
                .env
                .iter()
                .map(|(key, value)| format!("{key}={value}")),
        );
        return Ok((command, environment));
    }
    Err(failure(
        "This sign-in method is no longer available. Reopen Sign in to refresh it.",
    ))
}

fn authorize(state: &AppState, id: &str, headers: &HeaderMap) -> Result<AgentConfig, ApiError> {
    super::workspace::require_same_origin(headers)?;
    if headers.contains_key("x-xpressclaw-agent-id") {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Agent sign-in requires the browser session"})),
        ));
    }
    state
        .config()
        .agents
        .iter()
        .find(|agent| agent.name == id)
        .cloned()
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"Agent not found"})),
            )
        })
}

async fn prepare(
    state: &AppState,
    docker: &DockerManager,
    id: &str,
) -> Result<authentication::PreparedAuthentication, ApiError> {
    authentication::prepare(
        &state.db,
        &state.config,
        docker,
        id,
        &state.project_processes,
        &state.conversation_processes,
        &state.native_runtime_lifecycle,
        state.runner_callback_port.load(Ordering::Relaxed),
    )
    .await
    .map_err(failure)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{id}/authentication", get(status).post(discover))
        .route("/{id}/authentication/terminal", get(terminal))
        .route("/{id}/authentication/agent", get(agent_method))
}

async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let agent = authorize(&state, &id, &headers)?;
    let kind = resolve_runner_kind(&agent).map_err(failure)?;
    let auth = state
        .project_processes
        .connected_process(&id, &kind)
        .await
        .map(|process| process.authentication())
        .unwrap_or_default();
    Ok(Json(
        json!({"kind":kind,"status":auth.status,"methods":methods(&auth)}),
    ))
}

async fn discover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize(&state, &id, &headers)?;
    let docker = state
        .docker()
        .await
        .ok_or_else(|| failure("Docker or Podman is not available"))?;
    let prepared = prepare(&state, &docker, &id).await?;
    let auth = prepared.process.authentication();
    Ok(Json(
        json!({"kind":prepared.kind,"status":auth.status,"methods":methods(&auth)}),
    ))
}

#[derive(Deserialize)]
struct TerminalQuery {
    method_id: String,
    columns: Option<u16>,
    rows: Option<u16>,
}

async fn terminal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    authorize(&state, &id, &headers)?;
    let docker = state
        .docker()
        .await
        .ok_or_else(|| failure("Docker or Podman is not available"))?;
    Ok(upgrade.on_upgrade(move |socket| login(socket, state, docker, id, query)))
}

async fn agent_method(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TerminalQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    authorize(&state, &id, &headers)?;
    let docker = state
        .docker()
        .await
        .ok_or_else(|| failure("Docker or Podman is not available"))?;
    Ok(upgrade.on_upgrade(move |socket| agent_login(socket, state, docker, id, query.method_id)))
}

async fn finish_sign_in(
    prepared: &mut authentication::PreparedAuthentication,
    state: &AppState,
    docker: &DockerManager,
    id: &str,
    reconnect: bool,
) -> xpressclaw_core::error::Result<()> {
    authentication::finish(
        prepared,
        docker,
        id,
        &state.project_processes,
        &state.conversation_processes,
        reconnect,
    )
    .await?;
    let sessions = SessionManager::new(state.db.clone());
    sessions.ensure(id, None)?;
    sessions.append_event(
        id,
        NewEvent {
            attempt_id: None,
            task_id: None,
            source_type: "acp",
            source_id: Some(&prepared.kind),
            event_type: "authentication_succeeded",
            summary: "Agent sign-in completed",
            payload: json!({"runner":prepared.kind}),
        },
    )?;
    AgentRegistry::new(state.db.clone()).clear_error(id)?;
    Ok(())
}

async fn agent_login(
    mut socket: WebSocket,
    state: AppState,
    docker: Arc<DockerManager>,
    id: String,
    method_id: String,
) {
    let mut prepared = match prepare(&state, &docker, &id).await {
        Ok(prepared) => prepared,
        Err((_, detail)) => {
            let _ = socket
                .send(Message::Text(
                    json!({"type":"error","message":detail.0["error"]})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let Some(method) = methods(&prepared.process.authentication())
        .into_iter()
        .find(|method| method.id == method_id && method.kind == "agent")
    else {
        let _ = socket
            .send(Message::Text(
                json!({"type":"error","message":"This sign-in method is no longer available"})
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    };
    let start = match tokio::time::timeout(Duration::from_secs(30), socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) if text.len() <= 64 * 1024 => {
            serde_json::from_str::<Value>(&text).ok()
        }
        _ => None,
    };
    let Some(start) = start.filter(|value| value["type"] == "start") else {
        return;
    };
    let api_key = start["api_key"]
        .as_str()
        .filter(|key| !key.is_empty())
        .map(str::to_owned);
    let meta = if method.api_key {
        api_key
            .as_ref()
            .map(|key| serde_json::Map::from_iter([("api-key".into(), json!({"apiKey":key}))]))
    } else {
        None
    };
    let (broker, mut prompts) = AuthenticationBroker::new();
    let process = prepared.process.clone();
    let operation = process.authenticate(&method_id, meta, broker.clone());
    tokio::pin!(operation);
    let mut succeeded = false;
    let mut error = None;
    loop {
        tokio::select! {
            result = &mut operation => {
                match result { Ok(()) => succeeded=true, Err(cause) => error=Some(cause.to_string()) }
                break;
            }
            Some(prompt) = prompts.recv() => {
                if socket.send(Message::Text(json!({"type":"prompt","id":prompt.id,"request":prompt.request}).to_string().into())).await.is_err() { break; }
            }
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) if text.len() <= 64 * 1024 => {
                    if let Ok(value) = serde_json::from_str::<Value>(&text) {
                        if value["type"] == "response" && matches!(value["response"]["action"].as_str(), Some("accept"|"decline"|"cancel")) {
                            if let (Some(id),Ok(response)) = (value["id"].as_str(),serde_json::from_value::<CreateElicitationResponse>(value["response"].clone())) {
                                broker.respond(id,response);
                            }
                        }
                    }
                }
                Some(Ok(Message::Ping(bytes))) => { if socket.send(Message::Pong(bytes)).await.is_err() { break; } }
                Some(Ok(Message::Pong(_))) => {},
                _ => break,
            },
        }
    }
    broker.cancel();
    if !succeeded {
        authentication::cancel(&id, &state.project_processes).await;
    }
    if succeeded {
        match finish_sign_in(&mut prepared, &state, &docker, &id, false).await {
            Ok(()) => {
                let _ = socket
                    .send(Message::Text(
                        json!({"type":"authenticated"}).to_string().into(),
                    ))
                    .await;
                return;
            }
            Err(cause) => error = Some(cause.to_string()),
        }
    }
    if let Some(mut message) = error {
        if let Some(key) = api_key {
            message = message.replace(&key, "[redacted]");
        }
        let _ = socket
            .send(Message::Text(
                json!({"type":"error","message":message}).to_string().into(),
            ))
            .await;
    }
}

async fn login(
    mut socket: WebSocket,
    state: AppState,
    docker: Arc<DockerManager>,
    id: String,
    query: TerminalQuery,
) {
    let mut prepared = match prepare(&state, &docker, &id).await {
        Ok(prepared) => prepared,
        Err((_, detail)) => {
            let _ = socket
                .send(Message::Text(
                    json!({"type":"error","message":detail.0["error"]})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let (command, environment) = match login_command(&prepared, &query.method_id) {
        Ok(command) => command,
        Err((_, detail)) => {
            let _ = socket
                .send(Message::Text(
                    json!({"type":"error","message":detail.0["error"]})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let login = match docker
        .open_authentication_terminal(
            &id,
            &command,
            &environment,
            prepared.spec.working_dir.as_deref(),
            query.columns.unwrap_or(100).clamp(20, 500),
            query.rows.unwrap_or(25).clamp(5, 300),
        )
        .await
    {
        Ok(login) => login,
        Err(error) => {
            let _ = socket
                .send(Message::Text(
                    json!({"type":"error","message":error.to_string()})
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        }
    };
    let name = login.socket;
    let exec_id = login.terminal.exec_id;
    let mut input = login.terminal.input;
    let mut output = login.terminal.output;
    let (mut sender, mut receiver) = socket.split();
    let mut outcome = None;
    let mut failure_message = None;
    let mut poll = tokio::time::interval(Duration::from_millis(500));
    if sender
        .send(Message::Text(json!({"type":"ready"}).to_string().into()))
        .await
        .is_ok()
    {
        loop {
            tokio::select! {
                value = output.next() => match value {
                    Some(Ok(value)) => { if sender.send(Message::Binary(value.into_bytes())).await.is_err() { break; } }
                    Some(Err(error)) => { failure_message = Some(error.to_string()); break; }
                    None => { failure_message = Some("The sign-in terminal disconnected".into()); break; }
                },
                message = receiver.next() => match message {
                    Some(Ok(Message::Binary(bytes))) => { if input.write_all(&bytes).await.is_err() { break; } }
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            if value["type"] == "resize" {
                                if let (Some(columns),Some(rows)) = (value["columns"].as_u64(),value["rows"].as_u64()) {
                                    let _ = docker.resize_terminal(&exec_id, columns.clamp(20,500) as u16, rows.clamp(5,300) as u16).await;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(bytes))) => { if sender.send(Message::Pong(bytes)).await.is_err() { break; } }
                    Some(Ok(Message::Pong(_))) => {},
                    _ => break,
                },
                _ = poll.tick() => match docker.authentication_terminal_exit(&id, &name).await {
                    Ok(Some(code)) => { outcome = Some(code); break; }
                    Ok(None) => {},
                    Err(error) => { failure_message = Some(error.to_string()); break; }
                }
            }
        }
    }
    let _ = input.shutdown().await;
    docker.close_authentication_terminal(&id, &name).await;
    if outcome == Some(0) {
        match finish_sign_in(&mut prepared, &state, &docker, &id, true).await {
            Ok(()) => {
                let _ = sender
                    .send(Message::Text(
                        json!({"type":"authenticated"}).to_string().into(),
                    ))
                    .await;
                return;
            }
            Err(error) => failure_message = Some(error.to_string()),
        }
    } else if let Some(code) = outcome {
        failure_message = Some(format!(
            "Sign-in did not complete (exit status {code}). Try again."
        ));
    }
    if let Some(message) = failure_message {
        let _ = sender
            .send(Message::Text(
                json!({"type":"error","message":message}).to_string().into(),
            ))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::{config::Config, db::Database};

    #[test]
    fn protocol_payload_logging_cannot_disclose_sign_in_input_at_trace_level() {
        use tracing_subscriber::prelude::*;
        #[derive(Clone, Default)]
        struct Writer(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Writer::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::filter_fn(
                xpressclaw_core::workers::acp::allow_log_metadata,
            ))
            .with(tracing_subscriber::EnvFilter::new("trace"))
            .with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_writer(move || writer.clone()),
            );
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target:"agent_client_protocol::jsonrpc::outgoing_actor",payload="fixture-secret","outgoing payload");
            tracing::trace!(target:"tungstenite::protocol",payload="fixture-secret","websocket input");
            tracing::trace!(target:"tokio_tungstenite",payload="fixture-secret","websocket frame");
            tracing::trace!("application trace kept");
        });
        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("application trace kept"));
        assert!(!log.contains("fixture-secret"));
    }

    #[test]
    fn sign_in_choices_expose_methods_without_launch_arguments_or_secrets() {
        let mut auth = AcpAuthentication::default();
        auth.methods = serde_json::from_value(json!([
            {"type":"terminal","id":"terminal-login","name":"Account","args":["--login","private-argument"],"env":{"TOKEN":"private-token"}},
            {"id":"api-key","name":"API key","_meta":{"api-key":{"provider":"openai"}}}
        ])).unwrap();
        let value = serde_json::to_value(methods(&auth)).unwrap();
        assert_eq!(value[0]["kind"], "terminal");
        assert_eq!(value[1]["kind"], "agent");
        assert_eq!(value[1]["api_key"], true);
        assert!(!value.to_string().contains("private-"));
        assert!(value[0].get("env").is_none());
        assert!(value[0].get("args").is_none());
    }

    #[tokio::test]
    async fn authentication_routes_are_browser_scoped_and_status_is_prompt_free() {
        let config = Config {
            agents: vec![AgentConfig {
                name: "test-agent".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let db = Arc::new(Database::open_memory().unwrap());
        let state = AppState::new(
            Arc::new(config),
            db.clone(),
            None,
            "/tmp/auth-route-test.yaml".into(),
            true,
        );
        let app = Router::new()
            .nest("/api/sessions", routes())
            .with_state(state);
        for (path, caller, origin, expected) in [
            ("test-agent", None, None, StatusCode::OK),
            ("missing", None, None, StatusCode::NOT_FOUND),
            (
                "test-agent",
                Some("test-agent"),
                None,
                StatusCode::FORBIDDEN,
            ),
            (
                "test-agent",
                None,
                Some("https://example.invalid"),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let mut request = Request::builder()
                .uri(format!("/api/sessions/{path}/authentication"))
                .header("host", "localhost");
            if let Some(caller) = caller {
                request = request.header("x-xpressclaw-agent-id", caller);
            }
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK {
                let value: Value = serde_json::from_slice(
                    &response.into_body().collect().await.unwrap().to_bytes(),
                )
                .unwrap();
                assert_eq!(value["status"], "unknown");
                assert_eq!(value["methods"], json!([]));
            }
        }
        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT count(*) FROM work_attempts", [], |row| row.get(0))
                    .map_err(xpressclaw_core::error::Error::from)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
