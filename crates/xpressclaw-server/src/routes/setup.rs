use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use tracing::{info, warn};
use xpressclaw_core::agents::presets::builtin_presets;
use xpressclaw_core::agents::registry::{AgentRegistry, RegisterAgent};
use xpressclaw_core::config::{AgentConfig, Config, LlmConfig, McpServerConfig};
use xpressclaw_core::llm::anthropic::AnthropicProvider;
use xpressclaw_core::llm::local::detect_ollama;
use xpressclaw_core::llm::openai::OpenAiProvider;
use xpressclaw_core::llm::router::LlmRouter;
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
        .route("/download-status", get(download_status))
        .route("/config", get(get_config))
}

/// Return the current live configuration (sanitized — no API keys).
async fn get_config(State(state): State<AppState>) -> Json<Value> {
    let config = state.config();
    Json(json!({
        "llm": {
            "default_provider": config.llm.default_provider,
            "has_openai_key": config.llm.openai_api_key.is_some(),
            "openai_base_url": config.llm.openai_base_url,
            "has_anthropic_key": config.llm.anthropic_api_key.is_some(),
            "local_model": config.llm.local_model,
            "local_base_url": config.llm.local_base_url,
        },
        "agents": config.agents.iter().map(|a| {
            let mut agent = json!({
                "name": a.name,
                "backend": a.backend,
                "role": a.role,
                "model": a.model,
                "tools": a.tools,
                "volumes": a.volumes,
            });
            if let Some(ref budget) = a.budget {
                agent["budget"] = json!({
                    "daily": budget.daily,
                    "monthly": budget.monthly,
                    "on_exceeded": budget.on_exceeded,
                });
            }
            if let Some(ref rl) = a.rate_limit {
                agent["rate_limit"] = json!({
                    "requests_per_minute": rl.requests_per_minute,
                    "tokens_per_minute": rl.tokens_per_minute,
                    "concurrent_requests": rl.concurrent_requests,
                });
            }
            if !a.wake_on.is_empty() {
                agent["wake_on"] = json!(a.wake_on.iter().map(|w| json!({
                    "schedule": w.schedule,
                    "event": w.event,
                })).collect::<Vec<_>>());
            }
            agent
        }).collect::<Vec<_>>(),
        "system": {
            "budget": {
                "daily": config.system.budget.daily,
                "monthly": config.system.budget.monthly,
                "on_exceeded": config.system.budget.on_exceeded,
            },
        },
        "mcp_servers": config.mcp_servers.keys().collect::<Vec<_>>(),
    }))
}

/// Check whether setup has been completed.
async fn setup_status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "setup_complete": state.is_setup_complete() }))
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
    Json(json!(builtin_presets()))
}

#[derive(Deserialize)]
struct CompleteSetupRequest {
    llm: LlmSetup,
    #[serde(default)]
    agents: Vec<AgentSetup>,
    #[serde(default)]
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
    /// Isolation mode: "docker" (default) or "none" (containerless).
    #[serde(default = "default_isolation")]
    isolation: String,
}

fn default_isolation() -> String {
    "docker".into()
}

#[derive(Deserialize)]
struct LlmSetup {
    provider: String,
    api_key: Option<String>,
    base_url: Option<String>,
    local_model: Option<String>,
    local_base_url: Option<String>,
    /// If true, download the GGUF model and use embedded llama.cpp.
    /// Set when Ollama is not available.
    #[serde(default)]
    use_embedded: bool,
}

#[derive(Deserialize)]
struct AgentSetup {
    name: String,
    preset: Option<String>,
    role: Option<String>,
    backend: Option<String>,
    tools: Option<Vec<String>>,
}

/// Return current GGUF download progress.
async fn download_status(State(state): State<AppState>) -> Json<Value> {
    #[cfg(feature = "local-llm")]
    {
        let dp = state.download_progress.read().unwrap().clone();
        Json(json!(dp))
    }
    #[cfg(not(feature = "local-llm"))]
    {
        let _ = state;
        Json(json!({ "status": "Idle" }))
    }
}

/// Save the setup configuration and mark setup as complete.
async fn complete_setup(
    State(state): State<AppState>,
    Json(req): Json<CompleteSetupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let is_local = req.llm.provider == "local" || req.llm.provider == "ollama";
    let needs_download = is_local && req.llm.use_embedded;

    // Resolve GGUF source if needed (for config, even before download completes)
    #[cfg(feature = "local-llm")]
    let (gguf_repo, gguf_file) = if needs_download {
        let model_name = req
            .llm
            .local_model
            .as_deref()
            .unwrap_or(xpressclaw_core::llm::llamacpp::DEFAULT_GGUF_FILE);
        let (r, f) = resolve_gguf_source(model_name);
        (r.to_string(), f.to_string())
    } else {
        (String::new(), String::new())
    };

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
        local_model: if is_local {
            req.llm
                .local_model
                .clone()
                .or(Some("qwen3.5:latest".into()))
        } else {
            None
        },
        // Model path will be set after download completes
        local_model_path: None,
        local_base_url: if is_local {
            req.llm.local_base_url.clone()
        } else {
            None
        },
        ..Default::default()
    };

    // Agents
    let presets = builtin_presets();
    let agents = if req.agents.is_empty() {
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
                    .and_then(|id| presets.iter().find(|p| p.id == id));

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

    let mut config = Config {
        llm,
        agents,
        mcp_servers: req.mcp_servers,
        ..Default::default()
    };
    config.system.isolation = req.isolation.clone();

    // Save config to disk
    config.save(&state.config_path).map_err(internal_error)?;
    info!(path = %state.config_path.display(), "saved configuration");

    // Apply config immediately — register agents and build LLM router
    let config = Arc::new(config);

    // Register agents in the database
    let registry = AgentRegistry::new(state.db.clone());
    for agent_config in &config.agents {
        let mut agent_json = serde_json::Map::new();
        if !agent_config.role.is_empty() {
            agent_json.insert(
                "role".into(),
                serde_json::Value::String(agent_config.role.clone()),
            );
        }
        if let Some(ref model) = agent_config.model {
            agent_json.insert("model".into(), serde_json::Value::String(model.clone()));
        }

        match registry.register(&RegisterAgent {
            name: agent_config.name.clone(),
            backend: agent_config.backend.clone(),
            config: serde_json::Value::Object(agent_json),
        }) {
            Ok(record) => info!(
                name = record.name,
                backend = record.backend,
                "registered agent"
            ),
            Err(e) => warn!(name = agent_config.name, error = %e, "failed to register agent"),
        }
    }

    // Build LLM router from the new config
    let llm_router = LlmRouter::build_from_config(&config.llm);
    state.apply_config(config, Some(Arc::new(llm_router)));
    info!("configuration applied — setup complete");

    // Handle embedded model download if needed
    #[cfg(feature = "local-llm")]
    if needs_download {
        use xpressclaw_core::llm::llamacpp::{
            download_gguf_with_progress, is_gguf_cached, DownloadStatus,
        };

        // Check cache first — skip download entirely if model is already cached
        if let Some(cached_path) = is_gguf_cached(&gguf_repo, &gguf_file) {
            info!(path = %cached_path.display(), "GGUF model already cached");

            // Update config with cached model path and rebuild router
            let old_config = state.config();
            let mut new_llm = old_config.llm.clone();
            new_llm.local_model_path = Some(cached_path.to_string_lossy().to_string());

            let new_config = Config {
                llm: new_llm,
                agents: old_config.agents.clone(),
                mcp_servers: old_config.mcp_servers.clone(),
                system: old_config.system.clone(),
                ..Default::default()
            };
            let _ = new_config.save(&state.config_path);

            let new_config = Arc::new(new_config);
            let router = LlmRouter::build_from_config(&new_config.llm);
            state.apply_config(new_config, Some(Arc::new(router)));

            return Ok(Json(json!({
                "success": true,
                "downloading": false,
                "config_path": state.config_path.display().to_string()
            })));
        }

        // Not cached — spawn background download with progress tracking
        let progress = state.download_progress.clone();
        let state_clone = state.clone();
        let config_path = state.config_path.clone();

        {
            let mut dp = progress.write().unwrap();
            dp.status = DownloadStatus::Downloading;
            dp.filename = gguf_file.clone();
        }

        tokio::task::spawn_blocking(move || {
            match download_gguf_with_progress(&gguf_repo, &gguf_file, progress.clone()) {
                Ok(path) => {
                    info!(path = %path.display(), "GGUF download complete");

                    let old_config = state_clone.config();
                    let mut new_llm = old_config.llm.clone();
                    new_llm.local_model_path = Some(path.to_string_lossy().to_string());

                    let new_config = Config {
                        llm: new_llm,
                        agents: old_config.agents.clone(),
                        mcp_servers: old_config.mcp_servers.clone(),
                        system: old_config.system.clone(),
                        ..Default::default()
                    };
                    let _ = new_config.save(&config_path);

                    let new_config = Arc::new(new_config);
                    let router = LlmRouter::build_from_config(&new_config.llm);
                    state_clone.apply_config(new_config, Some(Arc::new(router)));
                }
                Err(e) => {
                    warn!(error = %e, "GGUF download failed");
                    let mut dp = progress.write().unwrap();
                    dp.status = DownloadStatus::Error;
                    dp.error = Some(e.to_string());
                }
            }
        });

        return Ok(Json(json!({
            "success": true,
            "downloading": true,
            "config_path": state.config_path.display().to_string()
        })));
    }

    Ok(Json(json!({
        "success": true,
        "downloading": false,
        "config_path": state.config_path.display().to_string()
    })))
}

/// Map a model name from the setup UI to a HuggingFace GGUF repo and filename.
///
/// The setup wizard shows model names like "qwen3.5:4b" (Ollama-style).
/// This maps them to the corresponding HuggingFace GGUF repo/file.
///
/// Available Qwen3.5 models:
/// - Dense: 0.8B, 4B, 9B, 27B
/// - MoE: 35B-A3B, 122B-A10B, 397B-A17B
#[cfg(feature = "local-llm")]
fn resolve_gguf_source(model_name: &str) -> (&str, &str) {
    match model_name {
        // Dense models
        s if s.contains("0.8") => ("unsloth/Qwen3.5-0.8B-GGUF", "Qwen3.5-0.8B-UD-Q4_K_XL.gguf"),
        s if s.contains("4b") => ("unsloth/Qwen3.5-4B-GGUF", "Qwen3.5-4B-UD-Q4_K_XL.gguf"),
        s if s.contains("9b") => ("unsloth/Qwen3.5-9B-GGUF", "Qwen3.5-9B-UD-Q4_K_XL.gguf"),
        s if s.contains("27b") => ("unsloth/Qwen3.5-27B-GGUF", "Qwen3.5-27B-UD-Q4_K_XL.gguf"),
        // MoE models (user must explicitly select)
        s if s.contains("35b") || s.contains("a3b") => (
            "unsloth/Qwen3.5-35B-A3B-GGUF",
            "Qwen3.5-35B-A3B-UD-Q4_K_XL.gguf",
        ),
        s if s.contains("122b") || s.contains("a10b") => (
            "unsloth/Qwen3.5-122B-A10B-GGUF",
            "Qwen3.5-122B-A10B-UD-Q4_K_XL.gguf",
        ),
        s if s.contains("397b") || s.contains("a17b") => (
            "unsloth/Qwen3.5-397B-A17B-GGUF",
            "Qwen3.5-397B-A17B-UD-Q4_K_XL.gguf",
        ),
        // If it's already a .gguf filename, use the default repo
        s if s.ends_with(".gguf") => (
            xpressclaw_core::llm::llamacpp::DEFAULT_GGUF_REPO,
            model_name,
        ),
        // Default: 4B is the safe default for most systems
        _ => ("unsloth/Qwen3.5-4B-GGUF", "Qwen3.5-4B-UD-Q4_K_XL.gguf"),
    }
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
        let state = AppState::new(config, db, None, test_config_path(), false);

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
