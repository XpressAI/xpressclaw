use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, EXPIRES,
    PRAGMA, WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use bollard::container::{LogOutput, LogsOptions};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use xpressclaw_core::collaboration::gitbucket::GitBucketProvider;
use xpressclaw_core::collaboration::jenkins::JenkinsProvider;
use xpressclaw_core::collaboration::stack::{
    CollaborationServiceStatus, CollaborationStack, CollaborationStackStatus, INSTALLATION_LABEL,
};
use xpressclaw_core::collaboration::{
    network_name, resource_prefix, BuildProvider, BuildRequest, CollaborationConfig,
    CollaborationSecrets, ForgeProvider, GITBUCKET_INTERNAL_URL, JENKINS_INTERNAL_URL,
};

use crate::state::AppState;

const CAPABILITY_HEADER: &str = "x-xpressclaw-collaboration-token";
const AGENT_HEADER: &str = "x-xpressclaw-agent-id";
const RESET_CONFIRMATION: &str = "RESET LOCAL COLLABORATION";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(get_settings).put(put_settings))
        .route("/install", post(install))
        .route("/start", post(start))
        .route("/stop", post(stop))
        .route("/restart", post(restart))
        .route("/upgrade", post(upgrade))
        .route("/reset", post(reset))
        .route("/logs/{service}", get(logs))
        .route(
            "/agent/git/{owner}/{repository}/{*path}",
            get(agent_git_proxy).post(agent_git_proxy),
        )
        .route("/agent/{tool}", post(agent_tool))
}

#[derive(serde::Serialize)]
struct SettingsResponse {
    config: CollaborationConfig,
    status: CollaborationStackStatus,
    credentials_configured: bool,
    reset_confirmation: &'static str,
}

async fn get_settings(State(state): State<AppState>) -> Result<Json<SettingsResponse>, ApiError> {
    response(&state).await.map(Json)
}

async fn put_settings(
    State(state): State<AppState>,
    Json(mut collaboration): Json<CollaborationConfig>,
) -> Result<Json<SettingsResponse>, ApiError> {
    collaboration.validate().map_err(ApiError::bad_request)?;
    collaboration.authorized_agents.sort();
    collaboration.authorized_agents.dedup();

    // Use the same lifecycle-then-config lock order as Reset. This prevents a
    // saved port/image change from racing an Install or Upgrade that retained
    // the previous configuration snapshot, while keeping Agent deletion's
    // config-only critical section compatible with this route.
    let _lifecycle_guard = state.collaboration_lifecycle_lock.lock().await;
    let _config_guard = state.config_write_lock.lock().await;
    let current_config = state.config();
    let known_agents = current_config
        .agents
        .iter()
        .map(|agent| agent.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    if let Some(unknown) = collaboration
        .authorized_agents
        .iter()
        .find(|agent| !known_agents.contains(agent.as_str()))
    {
        return Err(ApiError::bad_request(format!(
            "unknown Agent {unknown:?}; remove it from Local collaboration access"
        )));
    }

    let mut config = (*current_config).clone();
    config.collaboration = collaboration;
    config
        .save(&state.config_path)
        .map_err(ApiError::internal)?;
    let config = std::sync::Arc::new(config);
    state.apply_config(
        config.clone(),
        Some(std::sync::Arc::new(
            xpressclaw_core::llm::router::LlmRouter::build_from_config(&config),
        )),
    );
    response(&state).await.map(Json)
}

async fn install(State(state): State<AppState>) -> Result<Json<SettingsResponse>, ApiError> {
    with_stack(&state, |stack| {
        Box::pin(async move { stack.install().await })
    })
    .await?;
    response(&state).await.map(Json)
}

async fn start(State(state): State<AppState>) -> Result<Json<SettingsResponse>, ApiError> {
    with_stack(&state, |stack| Box::pin(async move { stack.start().await })).await?;
    response(&state).await.map(Json)
}

async fn stop(State(state): State<AppState>) -> Result<Json<SettingsResponse>, ApiError> {
    with_stack(&state, |stack| Box::pin(async move { stack.stop().await })).await?;
    response(&state).await.map(Json)
}

async fn restart(State(state): State<AppState>) -> Result<Json<SettingsResponse>, ApiError> {
    with_stack(&state, |stack| {
        Box::pin(async move { stack.restart().await })
    })
    .await?;
    response(&state).await.map(Json)
}

async fn upgrade(State(state): State<AppState>) -> Result<Json<SettingsResponse>, ApiError> {
    with_stack(&state, |stack| {
        Box::pin(async move { stack.upgrade().await })
    })
    .await?;
    response(&state).await.map(Json)
}

#[derive(Deserialize)]
struct ResetRequest {
    confirmation: String,
}

async fn reset(
    State(state): State<AppState>,
    Json(request): Json<ResetRequest>,
) -> Result<Json<SettingsResponse>, ApiError> {
    if request.confirmation != RESET_CONFIRMATION {
        return Err(ApiError::bad_request(format!(
            "type {RESET_CONFIRMATION:?} to permanently remove both volumes and credentials"
        )));
    }
    // Revoke all Agent access before deleting credentials and Docker data.
    // If Docker cleanup later fails, ordinary Agent work still remains usable
    // and no stale assignment points at missing secrets.
    let _lifecycle_guard = state.collaboration_lifecycle_lock.lock().await;
    let _config_guard = state.config_write_lock.lock().await;
    let mut config = (*state.config()).clone();
    revoke_collaboration_access(&mut config);
    config
        .save(&state.config_path)
        .map_err(ApiError::internal)?;
    let config = std::sync::Arc::new(config);
    state.apply_config(
        config.clone(),
        Some(std::sync::Arc::new(
            xpressclaw_core::llm::router::LlmRouter::build_from_config(&config),
        )),
    );
    with_stack_locked(&state, |stack| Box::pin(async move { stack.reset().await })).await?;
    response(&state).await.map(Json)
}

fn revoke_collaboration_access(config: &mut xpressclaw_core::config::Config) {
    config.collaboration.enabled = false;
    config.collaboration.authorized_agents.clear();
}

async fn logs(
    State(state): State<AppState>,
    Path(service): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let config = state.config();
    let installation = state.db.installation_id().map_err(ApiError::internal)?;
    let prefix = resource_prefix(&installation);
    let name = match service.as_str() {
        "gitbucket" => format!("{prefix}-gitbucket"),
        "jenkins" => format!("{prefix}-jenkins"),
        _ => {
            return Err(ApiError::bad_request(
                "service must be gitbucket or jenkins",
            ))
        }
    };
    let docker = state
        .docker()
        .await
        .ok_or_else(|| ApiError::unavailable("Docker is not available"))?;
    let inspected = docker.inspect_by_name(&name).await.ok_or_else(|| {
        ApiError::bad_request(format!(
            "{service} is not installed; choose Install services first"
        ))
    })?;
    let owned = inspected
        .config
        .as_ref()
        .and_then(|container| container.labels.as_ref())
        .and_then(|labels| labels.get(INSTALLATION_LABEL))
        .is_some_and(|owner| owner == &installation);
    if !owned {
        return Err(ApiError::forbidden(format!(
            "Docker resource {name} is not managed by this XpressClaw installation"
        )));
    }
    let mut stream = docker.client().logs(
        &name,
        Some(LogsOptions::<String> {
            stdout: true,
            stderr: true,
            tail: "200".to_string(),
            ..Default::default()
        }),
    );
    let mut output = String::new();
    while let Some(item) = stream.next().await {
        match item.map_err(ApiError::internal)? {
            LogOutput::StdOut { message }
            | LogOutput::StdErr { message }
            | LogOutput::Console { message } => {
                output.push_str(&String::from_utf8_lossy(&message));
            }
            _ => {}
        }
    }
    // Do not expose bootstrap environment or credentials. Service logs should
    // not contain them, but redact known values as defense in depth.
    if let Some(secrets) =
        CollaborationSecrets::load(&config.system.data_dir).map_err(ApiError::internal)?
    {
        output = redact_collaboration_secrets(output, &secrets);
    }
    Ok(Json(json!({ "logs": output })))
}

fn redact_collaboration_secrets(mut output: String, secrets: &CollaborationSecrets) -> String {
    for secret in [
        secrets.gitbucket_root_password.as_str(),
        secrets.gitbucket_service_password.as_str(),
        secrets
            .gitbucket_service_token
            .as_deref()
            .unwrap_or_default(),
        secrets.jenkins_password.as_str(),
        secrets.agent_capability_token.as_str(),
    ] {
        if !secret.is_empty() {
            output = output.replace(secret, "[REDACTED]");
        }
    }
    output
}

async fn response(state: &AppState) -> Result<SettingsResponse, ApiError> {
    let config = state.config();
    let secrets = CollaborationSecrets::load(&config.system.data_dir)
        .map_err(ApiError::internal)?
        .is_some();
    let installation = state.db.installation_id().map_err(ApiError::internal)?;
    let status = match state.docker().await {
        Some(docker) => {
            CollaborationStack::new(
                &docker,
                &config.collaboration,
                &config.system.data_dir,
                &installation,
            )
            .status()
            .await
        }
        None => unavailable_status(
            &config.collaboration,
            &config.system.data_dir,
            &installation,
        ),
    };
    Ok(SettingsResponse {
        config: config.collaboration.clone(),
        status,
        credentials_configured: secrets,
        reset_confirmation: RESET_CONFIRMATION,
    })
}

fn unavailable_status(
    config: &CollaborationConfig,
    data_dir: &std::path::Path,
    installation: &str,
) -> CollaborationStackStatus {
    fn service(
        image: &str,
        host_url: String,
        internal_url: &str,
        volume: String,
    ) -> CollaborationServiceStatus {
        CollaborationServiceStatus {
            state: "unavailable".to_string(),
            health: "unknown".to_string(),
            version: image.rsplit(':').next().unwrap_or("unknown").to_string(),
            image: image.to_string(),
            host_url,
            internal_url: internal_url.to_string(),
            volume,
            error: Some(
                "Docker is unavailable. Start Docker Desktop or Docker Engine.".to_string(),
            ),
        }
    }
    let prefix = resource_prefix(installation);
    CollaborationStackStatus {
        configured: config.enabled,
        docker_available: false,
        network: network_name(installation),
        data_path: data_dir.join("collaboration").display().to_string(),
        gitbucket: service(
            &config.gitbucket_image,
            config.gitbucket_url(),
            GITBUCKET_INTERNAL_URL,
            format!("{prefix}-gitbucket-data"),
        ),
        jenkins: service(
            &config.jenkins_image,
            config.jenkins_url(),
            JENKINS_INTERNAL_URL,
            format!("{prefix}-jenkins-data"),
        ),
    }
}

async fn with_stack<F>(state: &AppState, operation: F) -> Result<CollaborationStackStatus, ApiError>
where
    F: for<'a> FnOnce(
        CollaborationStack<'a>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = xpressclaw_core::error::Result<CollaborationStackStatus>,
                > + Send
                + 'a,
        >,
    >,
{
    // Install/start/stop/restart/upgrade/reset all reconcile the same fixed
    // Docker resource names. Hold one installation-wide lock for the complete
    // operation so concurrent Settings requests cannot remove or recreate one
    // another's containers, network, volumes, or bootstrap helpers.
    let _lifecycle_guard = state.collaboration_lifecycle_lock.lock().await;
    with_stack_locked(state, operation).await
}

async fn with_stack_locked<F>(
    state: &AppState,
    operation: F,
) -> Result<CollaborationStackStatus, ApiError>
where
    F: for<'a> FnOnce(
        CollaborationStack<'a>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = xpressclaw_core::error::Result<CollaborationStackStatus>,
                > + Send
                + 'a,
        >,
    >,
{
    let docker = state
        .docker()
        .await
        .ok_or_else(|| ApiError::unavailable("Docker is unavailable"))?;
    let config = state.config();
    let installation = state.db.installation_id().map_err(ApiError::internal)?;
    operation(CollaborationStack::new(
        &docker,
        &config.collaboration,
        &config.system.data_dir,
        &installation,
    ))
    .await
    .map_err(ApiError::internal)
}

async fn agent_tool(
    State(state): State<AppState>,
    Path(tool): Path<String>,
    headers: HeaderMap,
    Json(arguments): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let config = state.config();
    let secrets = CollaborationSecrets::load(&config.system.data_dir)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::unavailable("Local collaboration is not installed"))?;
    let agent = authorize_agent(&headers, &config.collaboration, &secrets).ok_or_else(|| {
        ApiError::forbidden("this Agent does not have Local collaboration access")
    })?;
    let forge_token = secrets
        .gitbucket_service_token
        .as_deref()
        .ok_or_else(|| ApiError::unavailable("GitBucket setup is incomplete"))?;
    let forge = GitBucketProvider::new(
        &config.collaboration.gitbucket_url(),
        CollaborationStack::service_user(),
        forge_token,
    )
    .map_err(ApiError::internal)?;
    let builds = JenkinsProvider::new(
        &config.collaboration.jenkins_url(),
        "xpressclaw",
        &secrets.jenkins_password,
    )
    .map_err(ApiError::internal)?;

    let value = match tool.as_str() {
        "capabilities" => json!({
            "forge": forge.capabilities(),
            "builds": builds.capabilities(),
            "gitbucket_url": GITBUCKET_INTERNAL_URL,
            "jenkins_url": JENKINS_INTERNAL_URL,
            "owner": CollaborationStack::service_user(),
        }),
        "git-transport" => json!({
            "base_path": format!(
                "/api/settings/collaboration/agent/git/{}",
                CollaborationStack::service_user()
            ),
            "username": agent,
        }),
        "get-repository" => serde_json::to_value(
            forge
                .get_repository(
                    git_name(&arguments, "owner")?,
                    git_name(&arguments, "repository")?,
                )
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "create-repository" => serde_json::to_value(
            forge
                .create_repository(
                    git_name(&arguments, "name")?,
                    arguments
                        .get("private")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "get-issue" => serde_json::to_value(
            forge
                .get_issue(
                    git_name(&arguments, "owner")?,
                    git_name(&arguments, "repository")?,
                    required_u64(&arguments, "number")?,
                )
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "create-issue" => serde_json::to_value(
            forge
                .create_issue(
                    git_name(&arguments, "owner")?,
                    git_name(&arguments, "repository")?,
                    required(&arguments, "title")?,
                    arguments.get("body").and_then(Value::as_str).unwrap_or(""),
                )
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "get-pull-request" => serde_json::to_value(
            forge
                .get_pull_request(
                    git_name(&arguments, "owner")?,
                    git_name(&arguments, "repository")?,
                    required_u64(&arguments, "number")?,
                )
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "create-pull-request" => serde_json::to_value(
            forge
                .create_pull_request(
                    git_name(&arguments, "owner")?,
                    git_name(&arguments, "repository")?,
                    required(&arguments, "title")?,
                    arguments.get("body").and_then(Value::as_str).unwrap_or(""),
                    required(&arguments, "head")?,
                    arguments
                        .get("base")
                        .and_then(Value::as_str)
                        .unwrap_or("main"),
                )
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "comment-pull-request" => {
            forge
                .comment_on_pull_request(
                    git_name(&arguments, "owner")?,
                    git_name(&arguments, "repository")?,
                    required_u64(&arguments, "number")?,
                    required(&arguments, "body")?,
                )
                .await
                .map_err(ApiError::tool)?;
            json!({ "status": "commented" })
        }
        "trigger-build" => serde_json::to_value(
            builds
                .trigger(&BuildRequest {
                    repository: required(&arguments, "repository")?.to_string(),
                    git_ref: required(&arguments, "git_ref")?.to_string(),
                })
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "get-build" => serde_json::to_value(
            builds
                .get(required_u64(&arguments, "number")?)
                .await
                .map_err(ApiError::tool)?,
        )
        .map_err(ApiError::internal)?,
        "build-logs" => json!({
            "logs": builds
                .logs(
                    required_u64(&arguments, "number")?,
                    arguments
                        .get("max_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(100_000) as usize,
                )
                .await
                .map_err(ApiError::tool)?
        }),
        "cancel-build" => {
            builds
                .cancel(required_u64(&arguments, "number")?)
                .await
                .map_err(ApiError::tool)?;
            json!({ "status": "cancelled" })
        }
        _ => {
            return Err(ApiError::bad_request(format!(
                "unknown collaboration tool {tool:?}"
            )))
        }
    };
    Ok(Json(value))
}

/// Streams the narrow Git smart-HTTP surface through the control plane.
///
/// Agents authenticate with their revocable, identity-bound collaboration
/// capability. The long-lived GitBucket service token is added only to the
/// server-side upstream request and never enters an Agent container.
async fn agent_git_proxy(
    State(state): State<AppState>,
    Path((owner, repository, path)): Path<(String, String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let config = state.config();
    let secrets = CollaborationSecrets::load(&config.system.data_dir)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::unavailable("Local collaboration is not installed"))?;
    authorize_git_agent(request.headers(), &config.collaboration, &secrets).ok_or_else(|| {
        ApiError::unauthorized("this Agent does not have Local collaboration access")
    })?;

    let owner = validate_git_name(&owner, "owner")?;
    if owner != CollaborationStack::service_user() {
        return Err(ApiError::forbidden(
            "managed Git transport is restricted to the local service account",
        ));
    }
    let repository = validate_git_name(&repository, "repository")?;
    let (parts, body) = request.into_parts();
    let query = parts.uri.query();
    let has_body = parts.method == axum::http::Method::POST;
    let valid_operation = matches!(
        (parts.method.as_str(), path.as_str(), query),
        (
            "GET",
            "info/refs",
            Some("service=git-receive-pack" | "service=git-upload-pack")
        ) | ("POST", "git-receive-pack", None)
    );
    if !valid_operation {
        return Err(ApiError::bad_request(
            "managed Git transport supports only ref discovery and branch pushes",
        ));
    }

    let forge_token = secrets
        .gitbucket_service_token
        .as_deref()
        .ok_or_else(|| ApiError::unavailable("GitBucket setup is incomplete"))?;
    let mut upstream_url = format!(
        "{}/{owner}/{repository}.git/{path}",
        config.collaboration.gitbucket_url()
    );
    if let Some(query) = query {
        upstream_url.push('?');
        upstream_url.push_str(query);
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(ApiError::internal)?;
    let mut upstream = client
        .request(parts.method, upstream_url)
        .basic_auth(CollaborationStack::service_user(), Some(forge_token));
    for name in [ACCEPT, CONTENT_TYPE, CONTENT_ENCODING, CONTENT_LENGTH] {
        if let Some(value) = parts.headers.get(&name) {
            upstream = upstream.header(name, value);
        }
    }
    if let Some(value) = parts.headers.get("git-protocol") {
        upstream = upstream.header("git-protocol", value);
    }
    if has_body {
        upstream = upstream.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }
    let upstream = upstream.send().await.map_err(ApiError::tool)?;

    let status = upstream.status();
    let headers = upstream.headers().clone();
    let mut response = Response::builder().status(status);
    for name in [
        CONTENT_TYPE,
        CONTENT_ENCODING,
        CONTENT_LENGTH,
        CACHE_CONTROL,
        EXPIRES,
        PRAGMA,
    ] {
        if let Some(value) = headers.get(&name) {
            response = response.header(name, value);
        }
    }
    response
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(ApiError::internal)
}

fn authorize_git_agent(
    headers: &HeaderMap,
    config: &CollaborationConfig,
    secrets: &CollaborationSecrets,
) -> Option<String> {
    let encoded = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?
        .strip_prefix("Basic ")?;
    let credentials = STANDARD.decode(encoded).ok()?;
    let credentials = std::str::from_utf8(&credentials).ok()?;
    let (agent, token) = credentials.split_once(':')?;
    (config.agent_authorized(agent) && secrets.capability_token_matches_agent(agent, token))
        .then(|| agent.to_string())
}

fn authorize_agent<'a>(
    headers: &'a HeaderMap,
    config: &CollaborationConfig,
    secrets: &CollaborationSecrets,
) -> Option<&'a str> {
    let supplied = headers
        .get(CAPABILITY_HEADER)
        .and_then(|value| value.to_str().ok());
    let agent = headers
        .get(AGENT_HEADER)
        .and_then(|value| value.to_str().ok())?;
    (config.agent_authorized(agent)
        && supplied.is_some_and(|token| secrets.capability_token_matches_agent(agent, token)))
    .then_some(agent)
}

fn required<'a>(value: &'a Value, field: &str) -> Result<&'a str, ApiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request(format!("{field} is required")))
}

fn git_name<'a>(value: &'a Value, field: &str) -> Result<&'a str, ApiError> {
    let name = required(value, field)?;
    validate_git_name(name, field)
}

fn validate_git_name<'a>(name: &'a str, field: &str) -> Result<&'a str, ApiError> {
    if name.len() > 100
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character))
    {
        return Err(ApiError::bad_request(format!(
            "{field} may contain only letters, numbers, dot, underscore, and hyphen"
        )));
    }
    Ok(name)
}

fn required_u64(value: &Value, field: &str) -> Result<u64, ApiError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::bad_request(format!("{field} must be a positive integer")))
}

struct ApiError(StatusCode, Json<Value>);

impl ApiError {
    fn bad_request(message: impl std::fmt::Display) -> Self {
        Self(
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": message.to_string() })),
        )
    }

    fn forbidden(message: impl std::fmt::Display) -> Self {
        Self(
            StatusCode::FORBIDDEN,
            Json(json!({ "error": message.to_string() })),
        )
    }

    fn unauthorized(message: impl std::fmt::Display) -> Self {
        Self(
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": message.to_string() })),
        )
    }

    fn unavailable(message: impl std::fmt::Display) -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": message.to_string() })),
        )
    }

    fn tool(message: impl std::fmt::Display) -> Self {
        Self(
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": message.to_string() })),
        )
    }

    fn internal(message: impl std::fmt::Display) -> Self {
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": message.to_string() })),
        )
    }
}

impl axum::response::IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = self.0;
        let mut response = (status, self.1).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                WWW_AUTHENTICATE,
                "Basic realm=\"XpressClaw local forge\"".parse().unwrap(),
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{Body, Bytes};
    use axum::http::Request;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::config::{AgentConfig, Config};
    use xpressclaw_core::db::Database;

    use super::*;

    #[test]
    fn unavailable_status_is_actionable_and_never_claims_health() {
        let status = unavailable_status(
            &CollaborationConfig::default(),
            std::path::Path::new("/tmp/xpressclaw"),
            "installation",
        );
        assert!(!status.docker_available);
        assert_eq!(status.gitbucket.state, "unavailable");
        assert!(status
            .gitbucket
            .error
            .as_deref()
            .unwrap()
            .contains("Start Docker"));
    }

    #[test]
    fn destructive_reset_requires_exact_confirmation() {
        assert_eq!(RESET_CONFIRMATION, "RESET LOCAL COLLABORATION");
    }

    #[test]
    fn destructive_reset_disables_and_clears_agent_access() {
        let mut config = xpressclaw_core::config::Config::default();
        config.collaboration.enabled = true;
        config.collaboration.authorized_agents = vec!["assigned".to_string()];
        revoke_collaboration_access(&mut config);
        assert!(!config.collaboration.enabled);
        assert!(config.collaboration.authorized_agents.is_empty());
    }

    #[tokio::test]
    async fn assignment_validation_uses_the_locked_agent_snapshot() {
        let config_path = std::env::temp_dir().join(format!(
            "test-xpressclaw-collaboration-race-{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        let mut config = Config::default();
        config.agents.push(AgentConfig {
            name: "deleted-agent".to_string(),
            backend: "generic".to_string(),
            ..Default::default()
        });
        config.save(&config_path).unwrap();
        let state = AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            config_path.clone(),
            true,
        );
        let app = Router::new()
            .nest("/settings/collaboration", routes())
            .with_state(state.clone());
        let submitted = CollaborationConfig {
            authorized_agents: vec!["deleted-agent".to_string()],
            ..Default::default()
        };

        // Hold the shared writer lock so this request is queued at the same
        // boundary as a concurrent Agent deletion. The protected mutation
        // below then wins the lock and removes the Agent before PUT validates.
        let guard = state.config_write_lock.lock().await;
        let request = tokio::spawn(
            app.oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings/collaboration")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&submitted).unwrap()))
                    .unwrap(),
            ),
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
        let mut after_deletion = (*state.config()).clone();
        after_deletion
            .agents
            .retain(|agent| agent.name != "deleted-agent");
        after_deletion.collaboration.authorized_agents.clear();
        after_deletion.save(&config_path).unwrap();
        state.apply_config(Arc::new(after_deletion), None);
        drop(guard);

        let response = tokio::time::timeout(Duration::from_secs(2), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let saved = Config::load(&config_path).unwrap();
        assert!(!saved
            .collaboration
            .authorized_agents
            .contains(&"deleted-agent".to_string()));
        let _ = std::fs::remove_file(config_path);
    }

    #[tokio::test]
    async fn settings_save_waits_for_collaboration_lifecycle_reconciliation() {
        let config_path = std::env::temp_dir().join(format!(
            "test-xpressclaw-collaboration-lifecycle-race-{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        let config = Config::default();
        config.save(&config_path).unwrap();
        let state = AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            config_path.clone(),
            true,
        );
        let app = Router::new()
            .nest("/settings/collaboration", routes())
            .with_state(state.clone());
        let submitted = CollaborationConfig {
            gitbucket_port: 18_088,
            ..Default::default()
        };

        let lifecycle_guard = state.collaboration_lifecycle_lock.lock().await;
        let request = tokio::spawn(
            app.oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings/collaboration")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&submitted).unwrap()))
                    .unwrap(),
            ),
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
        assert!(
            Config::load(&config_path)
                .unwrap()
                .collaboration
                .gitbucket_port
                != submitted.gitbucket_port
        );

        drop(lifecycle_guard);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if Config::load(&config_path)
                    .unwrap()
                    .collaboration
                    .gitbucket_port
                    == submitted.gitbucket_port
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        request.abort();
        let _ = request.await;
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn agent_access_requires_assignment_and_an_identity_bound_capability() {
        let secrets = CollaborationSecrets::generate();
        let config = CollaborationConfig {
            enabled: true,
            authorized_agents: vec!["allowed".to_string(), "other".to_string()],
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(AGENT_HEADER, "allowed".parse().unwrap());
        assert!(authorize_agent(&headers, &config, &secrets).is_none());
        headers.insert(
            CAPABILITY_HEADER,
            secrets
                .capability_token_for_agent("allowed")
                .parse()
                .unwrap(),
        );
        assert_eq!(
            authorize_agent(&headers, &config, &secrets),
            Some("allowed")
        );

        // A retained token cannot impersonate another still-authorized Agent.
        headers.insert(AGENT_HEADER, "other".parse().unwrap());
        assert!(authorize_agent(&headers, &config, &secrets).is_none());
        headers.insert(
            CAPABILITY_HEADER,
            secrets.capability_token_for_agent("other").parse().unwrap(),
        );
        assert_eq!(authorize_agent(&headers, &config, &secrets), Some("other"));

        // Removing one assignment immediately revokes only that Agent.
        let revoked = CollaborationConfig {
            enabled: true,
            authorized_agents: vec!["other".to_string()],
            ..Default::default()
        };
        headers.insert(AGENT_HEADER, "allowed".parse().unwrap());
        headers.insert(
            CAPABILITY_HEADER,
            secrets
                .capability_token_for_agent("allowed")
                .parse()
                .unwrap(),
        );
        assert!(authorize_agent(&headers, &revoked, &secrets).is_none());
    }

    #[tokio::test]
    async fn git_transport_keeps_the_shared_forge_token_server_side() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.system.data_dir = directory.path().join("data");
        config.collaboration.enabled = true;
        config.collaboration.authorized_agents = vec!["allowed".to_string()];
        let mut secrets = CollaborationSecrets::generate();
        secrets.gitbucket_service_token = Some("shared-forge-token".to_string());
        secrets.save(&config.system.data_dir).unwrap();
        let capability = secrets.capability_token_for_agent("allowed");
        let state = AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            directory.path().join("xpressclaw.yaml"),
            true,
        );
        let app = Router::new()
            .nest("/settings/collaboration", routes())
            .with_state(state);

        let response = app
            .oneshot(
                Request::post("/settings/collaboration/agent/git-transport")
                    .header("content-type", "application/json")
                    .header(AGENT_HEADER, "allowed")
                    .header(CAPABILITY_HEADER, capability)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let raw = String::from_utf8(body.to_vec()).unwrap();
        assert!(!raw.contains("shared-forge-token"));
        let payload: Value = serde_json::from_str(&raw).unwrap();
        assert!(payload.get("token").is_none());
        assert_eq!(payload["username"], "allowed");
        assert_eq!(
            payload["base_path"],
            "/api/settings/collaboration/agent/git/xpressclaw-agent"
        );
    }

    #[tokio::test]
    async fn git_proxy_streams_with_server_credentials_and_rechecks_revocation() {
        let forwarded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let upstream = Router::new().route(
            "/xpressclaw-agent/demo.git/git-receive-pack",
            post({
                let forwarded = forwarded.clone();
                move |headers: HeaderMap, body: Bytes| {
                    let forwarded = forwarded.clone();
                    async move {
                        forwarded.lock().unwrap().push((
                            headers
                                .get(AUTHORIZATION)
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or_default()
                                .to_string(),
                            body.to_vec(),
                        ));
                        (
                            [(CONTENT_TYPE, "application/x-git-receive-pack-result")],
                            "proxy-result",
                        )
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.system.data_dir = directory.path().join("data");
        config.collaboration.enabled = true;
        config.collaboration.gitbucket_port = port;
        config.collaboration.authorized_agents = vec!["allowed".to_string()];
        let mut secrets = CollaborationSecrets::generate();
        secrets.gitbucket_service_token = Some("shared-forge-token".to_string());
        secrets.save(&config.system.data_dir).unwrap();
        let capability = secrets.capability_token_for_agent("allowed");
        let basic = STANDARD.encode(format!("allowed:{capability}"));
        let state = AppState::new(
            Arc::new(config.clone()),
            Arc::new(Database::open_memory().unwrap()),
            None,
            directory.path().join("xpressclaw.yaml"),
            true,
        );
        let app = Router::new()
            .nest("/settings/collaboration", routes())
            .with_state(state.clone());
        let request = || {
            Request::post(
                "/settings/collaboration/agent/git/xpressclaw-agent/demo/git-receive-pack",
            )
            .header(AUTHORIZATION, format!("Basic {basic}"))
            .header(CONTENT_TYPE, "application/x-git-receive-pack-request")
            .body(Body::from("pack-data"))
            .unwrap()
        };

        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/x-git-receive-pack-result"
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "proxy-result"
        );
        let expected_upstream = STANDARD.encode("xpressclaw-agent:shared-forge-token");
        assert_eq!(
            forwarded.lock().unwrap().as_slice(),
            &[(format!("Basic {expected_upstream}"), b"pack-data".to_vec())]
        );

        config.collaboration.authorized_agents.clear();
        state.apply_config(Arc::new(config), None);
        let response = app.oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().contains_key(WWW_AUTHENTICATE));
        assert_eq!(forwarded.lock().unwrap().len(), 1);
        upstream_task.abort();
    }

    #[test]
    fn service_logs_redact_every_generated_credential() {
        let mut secrets = CollaborationSecrets::generate();
        secrets.gitbucket_service_token = Some("forge-token".to_string());
        let raw = format!(
            "{} {} {} {} {}",
            secrets.gitbucket_root_password,
            secrets.gitbucket_service_password,
            secrets.gitbucket_service_token.as_deref().unwrap(),
            secrets.jenkins_password,
            secrets.agent_capability_token,
        );
        let redacted = redact_collaboration_secrets(raw, &secrets);
        assert_eq!(redacted.matches("[REDACTED]").count(), 5);
        assert!(!redacted.contains("forge-token"));
    }
}
