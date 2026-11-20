use std::sync::Arc;

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::Serialize;
use serde_json::{json, Value};
use xpressclaw_core::config::ContainerEngineAccess;
use xpressclaw_core::proxy::{
    service::{write_policy, ProxyService, ProxyStatus},
    AgentProxyMode, ProxyConfig,
};

use crate::state::AppState;

type ApiError = (StatusCode, Json<Value>);
fn error(status: StatusCode, message: impl std::fmt::Display) -> ApiError {
    (status, Json(json!({"error": message.to_string()})))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(get_settings).put(put_settings))
}

#[derive(Serialize)]
struct SettingsResponse {
    config: ProxyConfig,
    status: ProxyStatus,
    direct_agents: Vec<String>,
    host_engine_agents: Vec<String>,
}

async fn response(state: &AppState) -> Result<SettingsResponse, ApiError> {
    let config = state.config();
    let installation = state
        .db
        .installation_id()
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let status = if let Some(docker) = state.docker().await {
        ProxyService {
            docker: &docker,
            data_dir: &config.system.data_dir,
            installation: &installation,
        }
        .status()
        .await
        .map_err(|e| error(StatusCode::SERVICE_UNAVAILABLE, e))?
    } else {
        ProxyStatus::unavailable()
    };
    Ok(SettingsResponse {
        config: config.proxy.clone(),
        status,
        direct_agents: config
            .agents
            .iter()
            .filter(|a| a.runner.proxy == AgentProxyMode::Direct)
            .map(|a| a.name.clone())
            .collect(),
        host_engine_agents: config
            .agents
            .iter()
            .filter(|a| {
                a.runner.proxy == AgentProxyMode::Inherit
                    && a.runner.container_engine == ContainerEngineAccess::Host
            })
            .map(|a| a.name.clone())
            .collect(),
    })
}

async fn get_settings(State(state): State<AppState>) -> Result<Json<SettingsResponse>, ApiError> {
    response(&state).await.map(Json)
}

async fn put_settings(
    State(state): State<AppState>,
    Json(proxy): Json<ProxyConfig>,
) -> Result<Json<SettingsResponse>, ApiError> {
    proxy
        .validate()
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    let _config_guard = state.config_write_lock.lock().await;
    let current = state.config();
    if proxy.enabled {
        let conflicts = current
            .agents
            .iter()
            .filter(|a| {
                proxy.applies_to(a.runner.proxy)
                    && a.runner.container_engine == ContainerEngineAccess::Host
            })
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>();
        if !conflicts.is_empty() {
            return Err(error(StatusCode::BAD_REQUEST, format!("Set these Agents to Connect directly or disable their host container-engine access before enabling the proxy: {}", conflicts.join(", "))));
        }
    }
    let installation = state
        .db
        .installation_id()
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if let Some(docker) = state.docker().await {
        ProxyService {
            docker: &docker,
            data_dir: &current.system.data_dir,
            installation: &installation,
        }
        .apply(&proxy)
        .await
        .map_err(|e| {
            error(
                if matches!(e, xpressclaw_core::error::Error::ConfigValidation(_)) {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                e,
            )
        })?;
    } else if proxy.enabled {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Start Docker or Podman before enabling the proxy",
        ));
    } else {
        write_policy(&current.system.data_dir, &proxy)
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    let mut next = (*current).clone();
    next.proxy = proxy;
    if let Err(cause) = next.save(&state.config_path) {
        let _ = write_policy(&current.system.data_dir, &current.proxy);
        return Err(error(StatusCode::INTERNAL_SERVER_ERROR, cause));
    }
    state.apply_config(Arc::new(next), state.llm_router());
    response(&state).await.map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use xpressclaw_core::{
        config::{AgentConfig, Config, NativeRunnerConfig},
        db::Database,
    };

    fn state(directory: &std::path::Path) -> AppState {
        let mut config = Config {
            proxy: ProxyConfig::legacy_default(),
            ..Default::default()
        };
        config.system.data_dir = directory.to_path_buf();
        let path = directory.join("xpressclaw.yaml");
        config.save(&path).unwrap();
        AppState::new(
            Arc::new(config),
            Arc::new(Database::open_memory().unwrap()),
            None,
            path,
            true,
        )
    }

    #[tokio::test]
    async fn invalid_policy_does_not_change_saved_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let state = state(directory.path());
        let before = std::fs::read(&state.config_path).unwrap();
        let proxy = ProxyConfig {
            blocked_domains: vec!["https://example.com".into()],
            ..ProxyConfig::legacy_default()
        };
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/")
                    .method("PUT")
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&proxy).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read(&state.config_path).unwrap(), before);
        assert!(!state.config().proxy.enabled);
    }

    #[tokio::test]
    async fn host_engine_requires_explicit_direct_override() {
        let directory = tempfile::tempdir().unwrap();
        let state = state(directory.path());
        let mut config = (*state.config()).clone();
        config.agents.push(AgentConfig {
            name: "builder".into(),
            runner: NativeRunnerConfig {
                container_engine: ContainerEngineAccess::Host,
                ..Default::default()
            },
            ..Default::default()
        });
        state.apply_config(Arc::new(config), None);
        let response = routes()
            .with_state(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/")
                    .method("PUT")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&ProxyConfig::default()).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("builder"));
        assert!(!state.config().proxy.enabled);
    }
}
