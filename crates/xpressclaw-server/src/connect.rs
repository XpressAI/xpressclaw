use std::io::Write;
use std::path::Path;
use std::time::Duration;

use axum::extract::{Path as RoutePath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures_util::{stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use xpressclaw_core::connect::{Binding, Command, ConnectJournal};
use xpressclaw_core::conversations::runtime::ConversationTurnQueue;
use xpressclaw_core::workers::acp::AcpInterruptMode;

use crate::state::AppState;

const SETTINGS_FILE: &str = "xpress-ai-connect.json";
const AUTH_HEADER: &str = "X-XpressAI-Connect-Authorization";

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    base_url: String,
    name: String,
    instance_id: Option<String>,
    credential: String,
    enabled: bool,
    pairing: Option<Pairing>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pairing {
    id: String,
    device_code: String,
    user_code: String,
    verification_path: String,
    expires_at: String,
    interval: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairStatus {
    status: String,
    instance_id: Option<String>,
    credential: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lease {
    status: String,
    lease_until: Option<String>,
    cancel_requested: bool,
}

#[derive(Deserialize)]
struct StartPairing {
    base_url: String,
    name: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BindRequest {
    local_project_id: String,
    local_agent_id: String,
    project_id: String,
    agent_name: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(settings).delete(disconnect))
        .route("/pair", post(start_pairing))
        .route("/pair/poll", post(poll_pairing))
        .route("/projects", get(projects))
        .route("/bindings", post(bind))
        .route("/bindings/{id}", delete(unbind))
        .route("/conversations/{id}/tools", post(proxy_tool))
        .route("/commands/{id}/tools", post(proxy_execution_tool))
}

impl Settings {
    fn load(directory: &Path) -> Result<Self, ConnectError> {
        match std::fs::read(directory.join(SETTINGS_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| internal("Could not read Connect settings")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err(internal("Could not read Connect settings")),
        }
    }

    fn save(&self, directory: &Path) -> Result<(), ConnectError> {
        let save = || -> anyhow::Result<()> {
            std::fs::create_dir_all(directory)?;
            let mut file = tempfile::Builder::new()
                .prefix(".connect-")
                .tempfile_in(directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            #[cfg(windows)]
            xpressclaw_core::workers::native::set_windows_owner_only_acl(file.path(), false)?;
            file.write_all(&serde_json::to_vec(self)?)?;
            file.as_file().sync_all()?;
            file.persist(directory.join(SETTINGS_FILE))?;
            Ok(())
        };
        save().map_err(|_| internal("Could not save Connect settings"))
    }

    fn connected(&self) -> Result<&str, ConnectError> {
        if !self.enabled || self.credential.is_empty() {
            return Err(bad("Connect this instance first"));
        }
        self.instance_id
            .as_deref()
            .ok_or_else(|| bad("Connect this instance first"))
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ConnectError> {
        let base = validate_origin(&self.base_url)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| internal("Could not initialize Connect client"))?;
        let mut request = client.request(method, format!("{base}/api/connect/v1{path}"));
        if !self.credential.is_empty() {
            request = request.header(AUTH_HEADER, format!("Bearer {}", self.credential));
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| unavailable("Cannot reach Xpress AI"))?;
        if !response.status().is_success() {
            return Err(ConnectError(
                if response.status() == StatusCode::UNAUTHORIZED {
                    StatusCode::BAD_GATEWAY
                } else {
                    response.status()
                },
                match response.status().as_u16() {
                    401 | 403 => "Xpress AI denied this connection or project access",
                    404 => "This platform does not support Connect, or the resource is unavailable",
                    409 => "Xpress AI rejected a conflicting binding or command",
                    429 => "Xpress AI is busy; try again shortly",
                    _ => "Xpress AI could not process the Connect request",
                }
                .into(),
            ));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| unavailable("Could not read Xpress AI response"))?
        {
            if bytes.len() + chunk.len() > 32 * 1024 * 1024 {
                return Err(unavailable("Xpress AI response exceeded the size limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| unavailable("Xpress AI returned an incompatible response"))
    }
}

fn validate_origin(input: &str) -> Result<String, ConnectError> {
    let url =
        reqwest::Url::parse(input.trim()).map_err(|_| bad("Enter a valid Xpress AI origin"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(bad(
            "Use an HTTPS platform origin without credentials, paths, or query parameters",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn public_settings(state: &AppState, config: &Settings) -> Result<Value, ConnectError> {
    let bindings = match &config.instance_id {
        Some(instance) => ConnectJournal::new(state.db.clone())
            .bindings(instance)
            .map_err(core_error)?,
        None => vec![],
    };
    let pending = config.pairing.as_ref().map(|pairing| json!({
        "user_code": pairing.user_code, "verification_url": format!("{}{}", config.base_url, pairing.verification_path),
        "expires_at": pairing.expires_at, "interval": pairing.interval,
    }));
    Ok(
        json!({ "base_url": config.base_url, "name": config.name, "instance_id": config.instance_id,
        "enabled": config.enabled, "paired": !config.credential.is_empty(), "pairing": pending, "bindings": bindings }),
    )
}

async fn proxy_tool(
    State(state): State<AppState>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ConnectError> {
    let agent = headers
        .get("x-xpressclaw-agent-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| bad("Agent identity is required"))?;
    let config = Settings::load(&state.config().system.data_dir)?;
    let command = ConnectJournal::new(state.db.clone())
        .tool_command(config.connected()?, &id, agent, Utc::now().timestamp())
        .map_err(core_error)?;
    Ok(Json(
        config
            .request(
                reqwest::Method::POST,
                &format!("/instance/commands/{command}/tools"),
                Some(body),
            )
            .await?,
    ))
}

async fn proxy_execution_tool(
    State(state): State<AppState>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ConnectError> {
    let agent = headers
        .get("x-xpressclaw-agent-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| bad("Agent identity is required"))?;
    let config = Settings::load(&state.config().system.data_dir)?;
    let command = ConnectJournal::new(state.db.clone())
        .tool_execution_command(config.connected()?, &id, agent, Utc::now().timestamp())
        .map_err(core_error)?;
    Ok(Json(
        config
            .request(
                reqwest::Method::POST,
                &format!("/instance/commands/{command}/tools"),
                Some(body),
            )
            .await?,
    ))
}

async fn settings(State(state): State<AppState>) -> Result<Json<Value>, ConnectError> {
    let config = Settings::load(&state.config().system.data_dir)?;
    Ok(Json(public_settings(&state, &config)?))
}

async fn start_pairing(
    State(state): State<AppState>,
    Json(input): Json<StartPairing>,
) -> Result<Json<Value>, ConnectError> {
    let _guard = state.config_write_lock.lock().await;
    let saved = Settings::load(&state.config().system.data_dir)?;
    if saved.enabled || !saved.credential.is_empty() {
        return Err(bad("Disconnect the current account before pairing again"));
    }
    let name = input.name.trim();
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(bad("Enter an instance name of 1–128 characters"));
    }
    let mut config = Settings {
        base_url: validate_origin(&input.base_url)?,
        name: name.into(),
        ..Settings::default()
    };
    let pairing: Pairing = config
        .request(
            reqwest::Method::POST,
            "/pairings",
            Some(json!({
                "installationId": state.db.installation_id().map_err(core_error)?, "name": name,
            })),
        )
        .await?;
    if !pairing.verification_path.starts_with("/settings/connect?")
        || pairing.verification_path.starts_with("//")
        || uuid::Uuid::parse_str(&pairing.id).is_err()
        || pairing.device_code.len() > 128
    {
        return Err(unavailable("Xpress AI returned an invalid pairing session"));
    }
    config.pairing = Some(pairing);
    config.save(&state.config().system.data_dir)?;
    Ok(Json(public_settings(&state, &config)?))
}

async fn poll_pairing(State(state): State<AppState>) -> Result<Json<Value>, ConnectError> {
    let _guard = state.config_write_lock.lock().await;
    let mut config = Settings::load(&state.config().system.data_dir)?;
    let pairing = config
        .pairing
        .as_ref()
        .ok_or_else(|| bad("Start pairing first"))?;
    let result: PairStatus = config
        .request(
            reqwest::Method::POST,
            &format!("/pairings/{}/poll", pairing.id),
            Some(json!({"deviceCode": pairing.device_code})),
        )
        .await?;
    match result.status.as_str() {
        "pending" => {}
        "approved" => {
            let id = result
                .instance_id
                .ok_or_else(|| unavailable("Missing instance identity"))?;
            if uuid::Uuid::parse_str(&id).is_err() {
                return Err(unavailable("Invalid instance identity"));
            }
            let credential = result
                .credential
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .ok_or_else(|| unavailable("Missing Connect credential"))?;
            config.instance_id = Some(id);
            config.credential = credential;
            config.enabled = true;
            config.pairing = None;
        }
        "expired" | "collected" => {
            config.pairing = None;
            config.save(&state.config().system.data_dir)?;
            return Err(bad(
                "Pairing expired or was already collected; start pairing again",
            ));
        }
        _ => return Err(unavailable("Unsupported pairing status")),
    }
    config.save(&state.config().system.data_dir)?;
    Ok(Json(public_settings(&state, &config)?))
}

async fn projects(State(state): State<AppState>) -> Result<Json<Value>, ConnectError> {
    let config = Settings::load(&state.config().system.data_dir)?;
    config.connected()?;
    Ok(Json(
        config
            .request(reqwest::Method::GET, "/instance/projects", None)
            .await?,
    ))
}

async fn bind(
    State(state): State<AppState>,
    Json(input): Json<BindRequest>,
) -> Result<Json<Value>, ConnectError> {
    let _guard = state.config_write_lock.lock().await;
    let config = Settings::load(&state.config().system.data_dir)?;
    let instance = config.connected()?;
    let agent = xpressclaw_core::agents::registry::AgentRegistry::new(state.db.clone())
        .get(&input.local_agent_id)
        .map_err(core_error)?;
    if agent.project_id.as_deref() != Some(input.local_project_id.as_str()) {
        return Err(bad("Select an Agent in the local Project"));
    }
    ConnectJournal::new(state.db.clone())
        .validate_project_mapping(instance, &input.local_project_id, &input.project_id)
        .map_err(core_error)?;
    let runtime_config = state.config();
    let runtime = runtime_config
        .agents
        .iter()
        .find(|a| a.name == input.local_agent_id)
        .ok_or_else(|| bad("The local Agent has no runtime configuration"))?;
    if runtime
        .runner
        .mcp_servers
        .iter()
        .any(|name| name == "xpressclaw")
    {
        return Err(bad("Connect requires the bundled XpressClaw control tools"));
    }
    let binding: Binding = config
        .request(
            reqwest::Method::POST,
            "/instance/bindings",
            Some(serde_json::to_value(&input).map_err(|_| bad("Invalid binding"))?),
        )
        .await?;
    if binding.local_project_id != input.local_project_id
        || binding.local_agent_id != input.local_agent_id
        || binding.project_id != input.project_id
        || binding.agent_name != input.agent_name
    {
        return Err(unavailable("Xpress AI returned a different binding"));
    }
    ConnectJournal::new(state.db.clone())
        .bind(instance, &binding)
        .map_err(core_error)?;
    Ok(Json(public_settings(&state, &config)?))
}

async fn unbind(
    State(state): State<AppState>,
    RoutePath(id): RoutePath<String>,
) -> Result<Json<Value>, ConnectError> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(bad("Invalid binding"));
    }
    let _guard = state.config_write_lock.lock().await;
    let config = Settings::load(&state.config().system.data_dir)?;
    ConnectJournal::new(state.db.clone())
        .disable_binding(config.connected()?, &id)
        .map_err(core_error)?;
    let _: Value = config
        .request(
            reqwest::Method::DELETE,
            &format!("/instance/bindings/{id}"),
            None,
        )
        .await?;
    Ok(Json(public_settings(&state, &config)?))
}

async fn disconnect(State(state): State<AppState>) -> Result<Json<Value>, ConnectError> {
    let saved = {
        let _guard = state.config_write_lock.lock().await;
        let saved = Settings::load(&state.config().system.data_dir)?;
        if let Some(instance) = &saved.instance_id {
            let journal = ConnectJournal::new(state.db.clone());
            for binding in journal.bindings(instance).map_err(core_error)? {
                journal
                    .disable_binding(instance, &binding.id)
                    .map_err(core_error)?;
            }
        }
        Settings {
            base_url: saved.base_url.clone(),
            name: saved.name.clone(),
            ..Settings::default()
        }
        .save(&state.config().system.data_dir)?;
        saved
    };
    let revoked = if saved.credential.is_empty() {
        true
    } else {
        saved
            .request::<Value>(reqwest::Method::DELETE, "/instance", None)
            .await
            .is_ok()
    };
    Ok(Json(
        json!({"disconnected": true, "remote_revoked": revoked}),
    ))
}

pub async fn run(state: AppState) {
    loop {
        if let Err(error) = sync(&state).await {
            tracing::warn!(status = %error.0, "Xpress AI Connect synchronization deferred");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn sync(state: &AppState) -> Result<(), ConnectError> {
    let config = Settings::load(&state.config().system.data_dir)?;
    let Ok(instance) = config.connected() else {
        return Ok(());
    };
    let journal = ConnectJournal::new(state.db.clone());
    journal.collect(instance).map_err(core_error)?;
    let pending = journal.pending(instance).map_err(core_error)?;
    // Send heartbeats concurrently; an unavailable receipt must not prevent other
    // live turns from renewing their authorization.
    let mut receipts = stream::iter(pending.into_iter().map(|execution| {
        let config = &config;
        async move {
            let response = config
                .request::<Lease>(
                    reqwest::Method::POST,
                    &format!("/instance/commands/{}/receipt", execution.id),
                    Some(
                        serde_json::to_value(&execution.receipt)
                            .map_err(|_| internal("Could not encode receipt"))?,
                    ),
                )
                .await;
            Ok::<_, ConnectError>((execution, response))
        }
    }))
    .buffer_unordered(20);
    while let Some(result) = receipts.next().await {
        let (execution, response) = result?;
        let receipt = match response {
            Ok(receipt) => receipt,
            Err(_) => continue,
        };
        if matches!(
            receipt.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            if execution.receipt.status == "accepted" {
                cancel(state, &execution)?;
                journal.collect(instance).map_err(core_error)?;
            }
            journal
                .acknowledge(instance, &execution.id)
                .map_err(core_error)?;
        } else if receipt.cancel_requested {
            cancel(state, &execution)?;
        } else if execution.receipt.status == "accepted" {
            journal
                .renew(instance, &execution.id, lease_time(&receipt)?)
                .map_err(core_error)?;
        }
    }
    let commands: Vec<Command> = config
        .request(reqwest::Method::GET, "/instance/commands", None)
        .await?;
    let delivery_started = std::time::Instant::now();
    for command in commands {
        if delivery_started.elapsed() > Duration::from_secs(20) {
            break;
        }
        let _guard = state.config_write_lock.lock().await;
        let current = Settings::load(&state.config().system.data_dir)?;
        if !current.enabled
            || current.instance_id != config.instance_id
            || current.credential != config.credential
        {
            return Ok(());
        }
        if uuid::Uuid::parse_str(&command.id).is_err() {
            return Err(unavailable("Invalid platform command identity"));
        }
        if !journal.can_admit(instance, &command).map_err(core_error)? {
            continue;
        }
        let approved = journal
            .bindings(instance)
            .map_err(core_error)?
            .into_iter()
            .any(|binding| binding == command.binding && binding.active);
        if !approved && !command.cancel_requested {
            let _: Lease = config
                .request(
                    reqwest::Method::POST,
                    &format!("/instance/commands/{}/receipt", command.id),
                    Some(json!({"status": "failed", "error": "local_binding_unavailable"})),
                )
                .await?;
            continue;
        }
        let receipt: Lease = config
            .request(
                reqwest::Method::POST,
                &format!("/instance/commands/{}/receipt", command.id),
                Some(json!({"status": "accepted"})),
            )
            .await?;
        if matches!(
            receipt.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            continue;
        }
        let mut command = command;
        command.cancel_requested |= receipt.cancel_requested;
        let expiry = if command.cancel_requested {
            0
        } else {
            lease_time(&receipt)?
        };
        let execution = match journal.admit(instance, &command, expiry, Utc::now().timestamp()) {
            Ok(execution) => execution,
            Err(_) => {
                let _: Lease = config
                    .request(
                        reqwest::Method::POST,
                        &format!("/instance/commands/{}/receipt", command.id),
                        Some(json!({"status": "failed", "error": "local_admission_rejected"})),
                    )
                    .await?;
                continue;
            }
        };
        if command.cancel_requested {
            cancel(state, &execution)?;
        }
    }
    Ok(())
}

pub async fn watch_leases(state: AppState) {
    loop {
        let result: xpressclaw_core::error::Result<Vec<String>> = state.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT instance_id FROM connect_commands WHERE status = 'accepted'",
            )?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(ids)
        });
        if let Ok(instances) = result {
            let journal = ConnectJournal::new(state.db.clone());
            for instance in instances {
                if let Ok(pending) = journal.pending(&instance) {
                    for execution in pending {
                        if execution.receipt.status == "accepted"
                            && execution.lease_until <= Utc::now().timestamp()
                        {
                            let _ = stop(&state, &execution, true);
                        }
                    }
                }
                let _ = journal.collect(&instance);
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn cancel(
    state: &AppState,
    execution: &xpressclaw_core::connect::Execution,
) -> Result<(), ConnectError> {
    stop(state, execution, false)
}

fn stop(
    state: &AppState,
    execution: &xpressclaw_core::connect::Execution,
    lease_expired: bool,
) -> Result<(), ConnectError> {
    if execution.attempt_id.is_some() {
        if let Some(attempt) = ConnectJournal::new(state.db.clone())
            .stop_task(&execution.id, lease_expired.then(|| Utc::now().timestamp()))
            .map_err(core_error)?
        {
            state
                .turn_controls
                .request_interrupt(&attempt, AcpInterruptMode::Immediate);
            state.elicitations.cancel_attempt(&attempt);
        }
    }
    if let (Some(conversation), Some(turn)) = (&execution.conversation_id, &execution.turn_id) {
        let queue = ConversationTurnQueue::new(state.db.clone());
        let cancellation = if lease_expired {
            queue.expire_lease(conversation, turn, &execution.id, Utc::now().timestamp())
        } else {
            queue.cancel(conversation, turn)
        }
        .map_err(core_error)?;
        if cancellation.changed && cancellation.was_running {
            state
                .turn_controls
                .request_interrupt(turn, AcpInterruptMode::Immediate);
            state.elicitations.cancel_attempt(turn);
        }
    }
    Ok(())
}

fn lease_time(lease: &Lease) -> Result<i64, ConnectError> {
    let value = lease
        .lease_until
        .as_deref()
        .ok_or_else(|| unavailable("Execution lease is missing"))?;
    DateTime::parse_from_rfc3339(value)
        .map(|t| t.timestamp().min(Utc::now().timestamp() + 90))
        .map_err(|_| unavailable("Execution lease is invalid"))
}

#[derive(Debug)]
pub struct ConnectError(StatusCode, String);
impl IntoResponse for ConnectError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
fn bad(message: &str) -> ConnectError {
    ConnectError(StatusCode::BAD_REQUEST, message.into())
}
fn internal(message: &str) -> ConnectError {
    ConnectError(StatusCode::INTERNAL_SERVER_ERROR, message.into())
}
fn unavailable(message: &str) -> ConnectError {
    ConnectError(StatusCode::BAD_GATEWAY, message.into())
}
fn core_error(error: xpressclaw_core::error::Error) -> ConnectError {
    bad(&error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use xpressclaw_core::{agents::registry::AgentRegistry, config::Config, db::Database};

    #[test]
    fn origins_cannot_include_credentials_paths_or_insecure_remote_hosts() {
        for url in [
            "http://remote.example",
            "https://user:secret@example.com",
            "https://example.com/path",
            "https://example.com?token=x",
            "file:///tmp/test",
        ] {
            assert!(validate_origin(url).is_err(), "{url}");
        }
        assert_eq!(
            validate_origin("https://example.com/").unwrap(),
            "https://example.com"
        );
        assert!(validate_origin("http://127.0.0.1:1234").is_ok());
        let lease = Lease {
            status: "in_progress".into(),
            lease_until: Some("2099-01-01T00:00:00Z".into()),
            cancel_requested: false,
        };
        assert!(lease_time(&lease).unwrap() <= Utc::now().timestamp() + 90);
    }

    #[tokio::test]
    async fn conflicting_project_binding_is_rejected_before_remote_publication() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("atlas", "native")
            .unwrap();
        let mut app_config = Config::load_default().unwrap();
        app_config.system.data_dir = directory.path().to_path_buf();
        let state = AppState::new(
            Arc::new(app_config),
            db.clone(),
            None,
            "test.yaml".into(),
            true,
        );
        let binding = Binding {
            id: uuid::Uuid::new_v4().to_string(),
            local_project_id: "atlas".into(),
            local_agent_id: "atlas".into(),
            project_id: "original".into(),
            agent_name: "atlas".into(),
            generation: 1,
            active: true,
        };
        let journal = ConnectJournal::new(db);
        journal.bind("instance", &binding).unwrap();
        // No platform listener exists: preflight must fail before making a request.
        Settings {
            base_url: "http://127.0.0.1:1".into(),
            instance_id: Some("instance".into()),
            credential: "private".into(),
            enabled: true,
            ..Settings::default()
        }
        .save(directory.path())
        .unwrap();
        let error = bind(
            State(state),
            Json(BindRequest {
                local_project_id: "atlas".into(),
                local_agent_id: "atlas".into(),
                project_id: "different".into(),
                agent_name: "atlas".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error
            .1
            .contains("already bound to another platform project"));
        assert_eq!(journal.bindings("instance").unwrap(), vec![binding]);
    }

    #[tokio::test]
    async fn protocol_delivery_replays_one_local_turn_and_keeps_credentials_private() {
        protocol_delivery("chat_turn").await;
    }

    #[tokio::test]
    async fn protocol_task_delivery_uses_task_queue_and_returns_attempt_receipt() {
        protocol_delivery("task_turn").await;
    }

    async fn protocol_delivery(kind: &str) {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_memory().unwrap());
        AgentRegistry::new(db.clone())
            .ensure("atlas", "native")
            .unwrap();
        let mut app_config = Config::load_default().unwrap();
        app_config.system.data_dir = directory.path().to_path_buf();
        let state = AppState::new(
            Arc::new(app_config),
            db.clone(),
            None,
            "test.yaml".into(),
            true,
        );
        let binding = Binding {
            id: uuid::Uuid::new_v4().to_string(),
            local_project_id: "atlas".into(),
            local_agent_id: "atlas".into(),
            project_id: "cloud".into(),
            agent_name: "atlas".into(),
            generation: 1,
            active: true,
        };
        let command = Command {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            binding: binding.clone(),
            kind: kind.into(),
            work_id: uuid::Uuid::new_v4().to_string(),
            source_conversation_id: None,
            payload: xpressclaw_core::connect::TurnPayload {
                text: "Hello".into(),
                history: vec![],
            },
            expires_at: (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            cancel_requested: false,
        };
        let receipts = Arc::new(Mutex::new(Vec::<Value>::new()));
        let received = receipts.clone();
        let delivered = command.clone();
        let platform = Router::new()
            .route("/api/connect/v1/instance/commands", get(move |headers: HeaderMap| { let value = delivered.clone(); async move {
                assert_eq!(headers.get(AUTH_HEADER).unwrap(), "Bearer private-credential");
                Json(vec![value])
            }}))
            .route("/api/connect/v1/instance/commands/{id}/receipt", post(move |Json(body): Json<Value>| { let received = received.clone(); async move {
                received.lock().unwrap().push(body.clone());
                Json(json!({"status": if body["status"] == "accepted" { "in_progress" } else { "completed" }, "leaseUntil": (Utc::now() + chrono::Duration::seconds(90)).to_rfc3339(), "cancelRequested": false}))
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let platform_task =
            tokio::spawn(async move { axum::serve(listener, platform).await.unwrap() });
        let config = Settings {
            base_url: format!("http://{address}"),
            instance_id: Some("instance".into()),
            credential: "private-credential".into(),
            enabled: true,
            ..Settings::default()
        };
        config.save(directory.path()).unwrap();
        let journal = ConnectJournal::new(db.clone());
        journal.bind("instance", &binding).unwrap();
        let public = public_settings(&state, &config).unwrap().to_string();
        assert!(!public.contains("private-credential"));
        assert!(!public.contains("device_code"));
        sync(&state).await.unwrap();
        sync(&state).await.unwrap();
        let pending = journal.pending("instance").unwrap();
        assert_eq!(pending.len(), 1);
        if kind == "chat_turn" {
            let conversation = pending[0].conversation_id.as_ref().unwrap();
            assert!(journal
                .tool_command(
                    "instance",
                    conversation,
                    "other-agent",
                    Utc::now().timestamp()
                )
                .is_err());
            db.with_conn(|conn| {
                conn.execute(
                    "UPDATE conversation_turns SET status='completed' WHERE id=?1",
                    [pending[0].turn_id.as_ref().unwrap()],
                )
            })
            .unwrap();
        } else {
            assert!(pending[0].conversation_id.is_none());
            let queue = xpressclaw_core::tasks::queue::TaskQueue::new(db.clone());
            let item = queue.claim("atlas").unwrap().unwrap();
            assert_eq!(Some(&item.task_id), pending[0].task_id.as_ref());
            xpressclaw_core::sessions::SessionManager::new(db.clone())
                .transition_attempt(
                    item.attempt_id.as_deref().unwrap(),
                    "completed",
                    "Done",
                    Some("Finished assigned work"),
                    None,
                )
                .unwrap();
            queue.complete(item.id, "Finished assigned work").unwrap();
        }
        sync(&state).await.unwrap();
        assert!(journal.pending("instance").unwrap().is_empty());
        assert!(receipts
            .lock()
            .unwrap()
            .iter()
            .any(|r| r["status"] == "completed"));
        let turns: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM conversation_turns", [], |row| {
                    row.get(0)
                })
            })
            .unwrap();
        assert_eq!(turns, if kind == "chat_turn" { 1 } else { 0 });
        if kind == "task_turn" {
            assert!(receipts
                .lock()
                .unwrap()
                .iter()
                .any(|r| r["text"] == "Finished assigned work"));
        }
        platform_task.abort();
    }
}
