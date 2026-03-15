use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::presets::PRESETS;
use xpressclaw_core::config::{AgentConfig, Config, LlmConfig, McpServerConfig};
use xpressclaw_core::llm::anthropic::AnthropicProvider;
use xpressclaw_core::llm::local::detect_ollama;
use xpressclaw_core::llm::openai::OpenAiProvider;
use xpressclaw_core::system;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/status", get(setup_status))
        .route("/check-docker", get(check_docker))
        .route("/system-info", get(system_info))
        .route("/check-ollama", get(check_ollama))
        .route("/recommend-model", get(recommend_model))
        .route("/validate-key", post(validate_key))
        .route("/presets", get(get_presets))
        .route("/complete", post(complete_setup))
}

/// Check whether setup has been completed.
async fn setup_status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "setup_complete": state.setup_complete }))
}

/// Check if Docker/Podman is available.
async fn check_docker() -> Json<Value> {
    match xpressclaw_core::docker::manager::DockerManager::connect().await {
        Ok(_) => Json(json!({
            "available": true,
            "error": null
        })),
        Err(e) => Json(json!({
            "available": false,
            "error": e.to_string()
        })),
    }
}

/// Detect system hardware (RAM, CPU, GPU).
async fn system_info() -> Json<Value> {
    let info = system::detect();
    Json(json!(info))
}

/// Check if Ollama is running and list models.
async fn check_ollama() -> Json<Value> {
    let info = detect_ollama().await;
    Json(json!(info))
}

/// Recommend a local model based on system hardware.
async fn recommend_model() -> Json<Value> {
    let info = system::detect();
    let rec = system::recommend_model(&info);
    Json(json!(rec))
}

#[derive(Deserialize)]
struct ValidateKeyRequest {
    provider: String,
    api_key: String,
    base_url: Option<String>,
}

/// Validate an API key for a provider.
async fn validate_key(
    Json(req): Json<ValidateKeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = match req.provider.as_str() {
        "openai" => OpenAiProvider::validate_key(&req.api_key, req.base_url.as_deref()).await,
        "anthropic" => AnthropicProvider::validate_key(&req.api_key).await,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unknown provider: {}", req.provider) })),
            ));
        }
    };

    match result {
        Ok(valid) => Ok(Json(json!({ "valid": valid }))),
        Err(e) => Ok(Json(json!({ "valid": false, "error": e }))),
    }
}

/// Return available agent presets.
async fn get_presets() -> Json<Value> {
    Json(json!(PRESETS))
}

#[derive(Deserialize)]
struct CompleteSetupRequest {
    llm: LlmSetup,
    #[serde(default)]
    agents: Vec<AgentSetup>,
    #[serde(default)]
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

#[derive(Deserialize)]
struct LlmSetup {
    provider: String,
    api_key: Option<String>,
    base_url: Option<String>,
    local_model: Option<String>,
}

#[derive(Deserialize)]
struct AgentSetup {
    name: String,
    preset: Option<String>,
    role: Option<String>,
    backend: Option<String>,
    tools: Option<Vec<String>>,
}

/// Save the setup configuration and mark setup as complete.
async fn complete_setup(
    State(state): State<AppState>,
    Json(req): Json<CompleteSetupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let llm = LlmConfig {
        default_provider: req.llm.provider.clone(),
        openai_api_key: if req.llm.provider == "openai" {
            req.llm.api_key.clone()
        } else {
            None
        },
        openai_base_url: if req.llm.provider == "openai" {
            req.llm.base_url.clone()
        } else {
            None
        },
        anthropic_api_key: if req.llm.provider == "anthropic" {
            req.llm.api_key.clone()
        } else {
            None
        },
        local_model: if req.llm.provider == "local" || req.llm.provider == "ollama" {
            req.llm
                .local_model
                .clone()
                .or(Some("qwen3.5:latest".into()))
        } else {
            None
        },
        ..Default::default()
    };

    // Agents
    let agents = if req.agents.is_empty() {
        // Default agent if none specified
        vec![AgentConfig {
            name: "atlas".to_string(),
            backend: "generic".to_string(),
            role: "You are a helpful AI assistant.".to_string(),
            ..Default::default()
        }]
    } else {
        req.agents
            .iter()
            .map(|a| {
                let preset = a
                    .preset
                    .as_deref()
                    .and_then(|id| PRESETS.iter().find(|p| p.id == id));

                AgentConfig {
                    name: a.name.clone(),
                    backend: a
                        .backend
                        .clone()
                        .or(preset.map(|p| p.backend.to_string()))
                        .unwrap_or("generic".to_string()),
                    role: a
                        .role
                        .clone()
                        .or(preset.map(|p| p.role.to_string()))
                        .unwrap_or_default(),
                    tools: a
                        .tools
                        .clone()
                        .or(preset.map(|p| p.default_tools.iter().map(|s| s.to_string()).collect()))
                        .unwrap_or_default(),
                    ..Default::default()
                }
            })
            .collect()
    };

    let config = Config {
        llm,
        agents,
        mcp_servers: req.mcp_servers,
        ..Default::default()
    };

    // Save config
    config.save(&state.config_path).map_err(internal_error)?;

    Ok(Json(json!({
        "success": true,
        "config_path": state.config_path.display().to_string()
    })))
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;

    use super::*;

    fn test_config_path() -> std::path::PathBuf {
        std::env::temp_dir().join("test-xpressclaw-setup.yaml")
    }

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState {
            config,
            db,
            llm_router: None,
            config_path: test_config_path(),
            setup_complete: false,
        };

        Router::new().nest("/setup", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_setup_status() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["setup_complete"], false);
    }

    #[tokio::test]
    async fn test_system_info() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/system-info")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["total_memory_gb"].as_f64().unwrap() > 0.0);
        assert!(body["cpu_count"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_recommend_model() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/recommend-model")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["model"].as_str().is_some());
        assert!(body["all_options"].as_array().is_some());
    }

    #[tokio::test]
    async fn test_presets() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/setup/presets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let presets = body.as_array().unwrap();
        assert!(presets.len() >= 3);
    }

    #[tokio::test]
    async fn test_complete_setup() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "llm": {
                                "provider": "local",
                                "local_model": "qwen3.5:8b"
                            },
                            "agents": [
                                {
                                    "name": "atlas",
                                    "preset": "assistant"
                                }
                            ]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["success"], true);

        // Verify config was written
        let config_path = test_config_path();
        assert!(config_path.exists());
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.llm.default_provider, "local");
        assert_eq!(config.agents[0].name, "atlas");

        // Cleanup
        let _ = std::fs::remove_file(config_path);
    }
}
