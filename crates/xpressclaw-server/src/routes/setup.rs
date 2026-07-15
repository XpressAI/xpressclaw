use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use tracing::{info, warn};
use xpressclaw_core::agents::presets::builtin_presets;
use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::config::{
    default_native_runner_image, AgentConfig, AgentLlmConfig, Config, LlmConfig, McpServerConfig,
    NativeRunnerConfig,
};
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
        .route("/start-docker", post(start_docker))
        .route("/system-info", get(system_info))
        .route("/check-ollama", get(check_ollama))
        .route("/recommend-model", get(recommend_model))
        .route("/validate-key", post(validate_key))
        .route("/presets", get(get_presets))
        .route("/complete", post(complete_setup))
        .route("/add-session", post(add_session))
        // Compatibility alias for clients from before the native-session UI.
        .route("/add-agent", post(add_session))
        .route("/config", get(get_config))
        .route("/mcp-servers", get(list_mcp_servers))
        .route("/mcp-servers", post(upsert_mcp_server))
        .route(
            "/mcp-servers/{name}",
            axum::routing::delete(delete_mcp_server),
        )
}

/// Return the current live configuration (sanitized — no API keys).
async fn get_config(State(state): State<AppState>) -> Json<Value> {
    let config = state.config();
    // Per-agent providers — collect a sanitized summary for the frontend.
    // Each entry shows the agent's declared provider, real model, base_url,
    // and whether an API key is set (never the key itself).
    let providers: Vec<Value> = config
        .agents
        .iter()
        .filter_map(|a| {
            let l = a.llm.as_ref()?;
            Some(json!({
                "agent": a.name,
                "provider": l.provider,
                "model": l.model,
                "base_url": l.base_url,
                "has_api_key": l.api_key.is_some(),
            }))
        })
        .collect();
    Json(json!({
        "llm": {
            // No global LLM config anymore. We expose the per-agent breakdown
            // so the settings page can render a summary without needing to
            // re-fetch /agents.
            "providers": providers,
        },
        "agents": config.agents.iter().map(|a| {
            let mut agent = json!({
                "name": a.name,
                "backend": a.backend,
                "display_name": a.display_name,
                "role_title": a.role_title,
                "responsibilities": a.responsibilities,
                "avatar": a.avatar,
                "role": a.role,
                "model": a.effective_model(),
                // Full llm block (including api_key) — needed by the agent
                // profile editor. /api/setup/config is a local-only endpoint
                // that already exposed the api_key under the previous shape.
                "llm": a.llm.as_ref().map(|l| json!({
                    "provider": l.provider,
                    "model": l.model,
                    "api_key": l.api_key,
                    "base_url": l.base_url,
                })),
                "runner": a.runner,
                "tools": a.tools,
                "skills": a.skills,
                "volumes": a.volumes,
            });
            if let Some(ref budget) = a.budget {
                agent["budget"] = json!({
                    "daily": budget.daily,
                    "monthly": budget.monthly,
                    "per_task": budget.per_task,
                    "on_exceeded": budget.on_exceeded,
                    "fallback_model": budget.fallback_model,
                    "warn_at_percent": budget.warn_at_percent,
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
                    "condition": w.condition,
                })).collect::<Vec<_>>());
            }
            agent["hooks"] = json!({
                "before_message": a.hooks.before_message,
                "after_message": a.hooks.after_message,
            });
            if let Some(ref ip) = a.idle_prompt {
                agent["idle_prompt"] = json!(ip);
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
        "mcp_servers": config.mcp_servers.iter().map(|(name, cfg)| {
            json!({
                "name": name,
                "type": cfg.server_type,
                "command": cfg.command,
                "args": cfg.args,
                "url": cfg.url,
                "env": cfg.env.keys().collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    }))
}

/// Check whether setup has been completed.
async fn setup_status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "setup_complete": state.is_setup_complete() }))
}

/// Check if Docker/Podman is available.
async fn check_docker() -> Json<Value> {
    use xpressclaw_core::docker::manager::DockerManager;
    let available = DockerManager::connect().await.is_ok();
    let installed = DockerManager::is_docker_desktop_installed();
    Json(json!({
        "available": available,
        "installed": installed,
        "can_start": installed && !available,
    }))
}

/// Try to start Docker Desktop. Only works on macOS/Windows.
async fn start_docker() -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use xpressclaw_core::docker::manager::DockerManager;
    DockerManager::start_docker_desktop().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(Json(json!({ "started": true })))
}

/// Detect system hardware (RAM, CPU, GPU).
async fn system_info() -> Json<Value> {
    let info = system::detect();
    let mut value = json!(info);
    value["working_directory"] = json!(std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string()));
    Json(value)
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
        "anthropic" => AnthropicProvider::validate_key(&req.api_key, req.base_url.as_deref()).await,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unknown provider: {}", req.provider) })),
            ));
        }
    };

    match result {
        Ok(valid) => {
            if !valid {
                return Ok(Json(json!({ "valid": false, "error": "Invalid API key" })));
            }
            // Fetch available models from the provider
            let models =
                fetch_provider_models(&req.provider, &req.api_key, req.base_url.as_deref()).await;
            Ok(Json(json!({ "valid": true, "models": models })))
        }
        Err(e) => Ok(Json(json!({ "valid": false, "error": e }))),
    }
}

/// Fetch available models from a provider's API.
async fn fetch_provider_models(
    provider: &str,
    api_key: &str,
    base_url: Option<&str>,
) -> Vec<Value> {
    let client = reqwest::Client::new();
    let url = match provider {
        "openai" => {
            let base = base_url.unwrap_or("https://api.openai.com");
            format!("{}/v1/models", base.trim_end_matches('/'))
        }
        "anthropic" => {
            let base = base_url.unwrap_or("https://api.anthropic.com");
            format!("{}/v1/models", base.trim_end_matches('/'))
        }
        _ => return vec![],
    };

    let mut req = client.get(&url);
    match provider {
        "anthropic" => {
            req = req
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01");
        }
        _ => {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }
    }

    match req.timeout(std::time::Duration::from_secs(10)).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<Value>().await {
                if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
                    return data
                        .iter()
                        .filter_map(|m| {
                            let id = m.get("id")?.as_str()?;
                            Some(json!({ "id": id }))
                        })
                        .collect();
                }
            }
            vec![]
        }
        _ => vec![],
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
    /// Isolation mode. Native workers currently require "docker".
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
    /// Legacy Ollama-tag-style model name (e.g. "qwen3.5:4b") used when
    /// importing a pre-native-runner setup request.
    local_model: Option<String>,
    /// Base URL for the Ollama instance when `provider == "ollama"`.
    /// Defaults to http://localhost:11434.
    local_base_url: Option<String>,
}

#[derive(Deserialize)]
struct AgentSetup {
    name: String,
    preset: Option<String>,
    role: Option<String>,
    role_title: Option<String>,
    responsibilities: Option<String>,
    backend: Option<String>,
    runner_kind: Option<String>,
    runner_image: Option<String>,
    runner_workspace: Option<String>,
    subscription_auth: Option<bool>,
    model: Option<String>,
    tools: Option<Vec<String>>,
    volumes: Option<Vec<String>>,
    /// Legacy connector overrides accepted by the compatibility endpoint.
    #[serde(default)]
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

fn runner_from_setup(setup: &AgentSetup) -> NativeRunnerConfig {
    let kind = setup
        .runner_kind
        .clone()
        .unwrap_or_else(|| "auto".to_string());
    let image = setup
        .runner_image
        .as_deref()
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .map(str::to_owned)
        .or_else(|| default_native_runner_image(&kind).map(str::to_owned))
        .unwrap_or_default();
    NativeRunnerConfig {
        kind,
        image,
        workspace: setup
            .runner_workspace
            .as_deref()
            .map(str::trim)
            .filter(|workspace| !workspace.is_empty())
            .map(str::to_owned),
        subscription_auth: setup.subscription_auth.unwrap_or(true),
        ..Default::default()
    }
}

/// Save the setup configuration and mark setup as complete.
async fn complete_setup(
    State(state): State<AppState>,
    Json(req): Json<CompleteSetupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if req.isolation != "docker" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "native workers require Docker or Podman isolation" })),
        ));
    }
    // Global LLM config no longer holds provider/model/key — those moved
    // onto each agent. Only `custom_pricing` lives here.
    let llm = LlmConfig::default();

    // Agents
    let presets = builtin_presets();
    let agents = if req.agents.is_empty() {
        vec![AgentConfig {
            name: "atlas".to_string(),
            backend: "codex".to_string(),
            runner: NativeRunnerConfig {
                kind: "codex".to_string(),
                ..Default::default()
            },
            role: "You are a helpful AI assistant.".to_string(),
            ..Default::default()
        }]
    } else {
        let mut used_ids: Vec<String> = Vec::new();
        req.agents
            .iter()
            .map(|a| {
                let preset = a
                    .preset
                    .as_deref()
                    .and_then(|id| presets.iter().find(|p| p.id == id));

                let tools = a
                    .tools
                    .clone()
                    .or(preset.map(|p| p.default_tools.iter().map(|s| s.to_string()).collect()))
                    .unwrap_or_default();

                // Per-agent LLM config built from the wizard's selections.
                // Each agent gets its own provider/model/key/base_url so it
                // can later be edited independently.
                let agent_llm = {
                    let provider = req.llm.provider.clone();
                    if provider.is_empty() {
                        None
                    } else {
                        // Per-agent model: prefer the agent's own model from
                        // the wizard, fall back to the wizard's local_model
                        // (set when provider is local/ollama), then default.
                        let model = a
                            .model
                            .clone()
                            .or_else(|| req.llm.local_model.clone())
                            .or_else(|| {
                                if provider == "ollama" || provider == "local" {
                                    Some("qwen3.5:latest".into())
                                } else {
                                    None
                                }
                            });
                        let base_url = req.llm.base_url.clone().or_else(|| {
                            req.llm.local_base_url.clone().or_else(|| {
                                if provider == "ollama" {
                                    Some("http://localhost:11434".to_string())
                                } else {
                                    None
                                }
                            })
                        });
                        Some(AgentLlmConfig {
                            provider: Some(provider),
                            model,
                            api_key: req.llm.api_key.clone(),
                            base_url,
                        })
                    }
                };

                // Slugify the name for use as an ID, keep original as display_name
                let id_refs: Vec<&str> = used_ids.iter().map(|s| s.as_str()).collect();
                let agent_id = xpressclaw_core::config::unique_agent_id(&a.name, &id_refs);
                used_ids.push(agent_id.clone());

                AgentConfig {
                    name: agent_id,
                    display_name: Some(a.name.clone()),
                    role_title: a.role_title.clone(),
                    responsibilities: a.responsibilities.clone(),
                    backend: a
                        .runner_kind
                        .clone()
                        .or(a.backend.clone().or(preset.map(|p| p.backend.to_string())))
                        .unwrap_or("codex".to_string()),
                    role: a
                        .role
                        .clone()
                        .or(preset.map(|p| p.role.to_string()))
                        .unwrap_or_default(),
                    llm: agent_llm,
                    runner: runner_from_setup(a),
                    tools,
                    skills: Vec::new(),
                    volumes: a.volumes.clone().unwrap_or_default(),
                    ..Default::default()
                }
            })
            .collect()
    };

    let mut config = Config {
        llm,
        agents,
        // Native CLIs own their tool loop. Only keep connectors the user
        // explicitly configured; do not inject the retired agent-layer MCPs.
        mcp_servers: req.mcp_servers,
        ..Default::default()
    };
    config.system.isolation = req.isolation.clone();

    // Save config to disk
    config.save(&state.config_path).map_err(internal_error)?;
    info!(path = %state.config_path.display(), "saved configuration");

    // Apply config immediately — register agents and build LLM router
    let config = Arc::new(config);

    // Sync agents in the database to match the new config.
    // Remove any agents not in the new config, then register the new ones.
    let registry = AgentRegistry::new(state.db.clone());
    let sessions = xpressclaw_core::sessions::SessionManager::new(state.db.clone());
    let existing_agents = registry.list().unwrap_or_default();
    let new_agent_names: std::collections::HashSet<&str> =
        config.agents.iter().map(|a| a.name.as_str()).collect();
    for existing in &existing_agents {
        if !new_agent_names.contains(existing.name.as_str()) {
            info!(name = existing.name, "removing agent not in new config");
            let _ = registry.delete(&existing.id);
            let _ = sessions.delete(&existing.id);
        }
    }
    for agent_config in &config.agents {
        match registry.ensure(&agent_config.name, &agent_config.backend) {
            Ok(record) => {
                let title = agent_config
                    .display_name
                    .as_deref()
                    .unwrap_or(&agent_config.name);
                if let Err(error) = sessions.ensure(&record.id, Some(title)) {
                    warn!(name = record.name, error = %error, "failed to sync session");
                } else {
                    info!(
                        name = record.name,
                        backend = record.backend,
                        "synced native session"
                    );
                }
            }
            Err(e) => warn!(name = agent_config.name, error = %e, "failed to sync agent"),
        }
    }

    // Build LLM router from the new config
    let llm_router = LlmRouter::build_from_config(&config);
    state.apply_config(config, Some(Arc::new(llm_router)));
    info!("configuration applied — setup complete");

    Ok(Json(json!({
        "success": true,
        "downloading": false,
        "config_path": state.config_path.display().to_string()
    })))
}

/// Add a durable native session without replacing existing sessions.
async fn add_session(
    State(state): State<AppState>,
    Json(req): Json<AgentSetup>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let presets = builtin_presets();
    let preset = req
        .preset
        .as_deref()
        .and_then(|id| presets.iter().find(|p| p.id == id));

    let tools = req
        .tools
        .clone()
        .or(preset.map(|p| p.default_tools.iter().map(|s| s.to_string()).collect()))
        .unwrap_or_default();

    // Native products own model selection and authentication. Keep the old
    // per-agent LLM block empty for newly-created sessions.
    let agent_llm = None;

    // Slugify the name and ensure uniqueness
    let old_config = state.config();
    let existing_ids: Vec<&str> = old_config.agents.iter().map(|a| a.name.as_str()).collect();
    let agent_id = xpressclaw_core::config::unique_agent_id(&req.name, &existing_ids);

    let agent_config = AgentConfig {
        name: agent_id.clone(),
        display_name: Some(req.name.clone()),
        role_title: req.role_title.clone(),
        responsibilities: req.responsibilities.clone(),
        backend: req
            .runner_kind
            .clone()
            .or(req
                .backend
                .clone()
                .or(preset.map(|p| p.backend.to_string())))
            .unwrap_or("codex".to_string()),
        role: req
            .role
            .clone()
            .or(preset.map(|p| p.role.to_string()))
            .unwrap_or_default(),
        llm: agent_llm,
        runner: runner_from_setup(&req),
        tools,
        skills: Vec::new(),
        volumes: req.volumes.clone().unwrap_or_default(),
        ..Default::default()
    };

    // Append to existing config (don't replace)
    let old_config = state.config();
    let mut new_agents = old_config.agents.clone();

    // Replace if agent with same name exists, otherwise append
    if let Some(idx) = new_agents.iter().position(|a| a.name == agent_config.name) {
        new_agents[idx] = agent_config.clone();
    } else {
        new_agents.push(agent_config.clone());
    }

    // Preserve existing explicit connectors and merge only new explicit
    // overrides. Native CLIs do not consume the old built-in agent MCP layer.
    let mut new_mcp = old_config.mcp_servers.clone();
    for (name, server) in req.mcp_servers {
        new_mcp.insert(name, server);
    }

    let new_config = Config {
        agents: new_agents,
        llm: old_config.llm.clone(),
        mcp_servers: new_mcp,
        system: old_config.system.clone(),
        ..Default::default()
    };
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;
    info!(
        name = agent_config.name,
        "added native session to configuration"
    );

    // Register the durable profile/session. Native workers are started per
    // attempt, so there is no long-running agent to auto-start.
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry
        .ensure(&agent_config.name, &agent_config.backend)
        .map_err(internal_error)?;
    xpressclaw_core::sessions::SessionManager::new(state.db.clone())
        .ensure(&record.id, agent_config.display_name.as_deref())
        .map_err(internal_error)?;

    // Rebuild the router so the new agent has a binding. The previous
    // implementation reused the existing router, which silently meant the
    // newly-added agent had no provider mapping until the server restarted.
    let new_config = std::sync::Arc::new(new_config);
    let new_router = Arc::new(LlmRouter::build_from_config(&new_config));
    state.apply_config(new_config, Some(new_router));

    Ok(Json(json!({
        "success": true,
        "session": agent_config.name,
        "session_id": record.id,
        // Retained for older clients using /add-agent.
        "agent": agent_config.name,
    })))
}

// ---------------------------------------------------------------------------
// MCP server management
// ---------------------------------------------------------------------------

/// List all configured MCP servers with full details.
async fn list_mcp_servers(State(state): State<AppState>) -> Json<Value> {
    let config = state.config();
    let servers: Vec<Value> = config
        .mcp_servers
        .iter()
        .map(|(name, cfg)| {
            json!({
                "name": name,
                "type": cfg.server_type,
                "command": cfg.command,
                "args": cfg.args,
                "url": cfg.url,
                "env": cfg.env,
                "headers": cfg.headers,
            })
        })
        .collect();
    Json(json!({ "servers": servers }))
}

#[derive(Debug, Deserialize)]
struct UpsertMcpServerRequest {
    name: String,
    #[serde(rename = "type", default = "default_stdio")]
    server_type: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::HashMap<String, String>,
    url: Option<String>,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
}

fn default_stdio() -> String {
    "stdio".to_string()
}

/// Add or update an MCP server in the global config.
async fn upsert_mcp_server(
    State(state): State<AppState>,
    Json(req): Json<UpsertMcpServerRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let old_config = state.config();

    let mut new_mcp = old_config.mcp_servers.clone();
    new_mcp.insert(
        req.name.clone(),
        McpServerConfig {
            server_type: req.server_type,
            command: req.command,
            args: req.args,
            env: req.env,
            url: req.url,
            headers: req.headers,
        },
    );

    let new_config = Config {
        mcp_servers: new_mcp,
        agents: old_config.agents.clone(),
        llm: old_config.llm.clone(),
        system: old_config.system.clone(),
        tools: old_config.tools.clone(),
        tool_policies: old_config.tool_policies.clone(),
        memory: old_config.memory.clone(),
    };
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;

    let new_config = std::sync::Arc::new(new_config);
    state.apply_config(new_config, state.llm_router());

    Ok(Json(json!({ "success": true, "name": req.name })))
}

/// Delete an MCP server from the global config.
async fn delete_mcp_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let old_config = state.config();

    let mut new_mcp = old_config.mcp_servers.clone();
    if new_mcp.remove(&name).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("MCP server '{name}' not found") })),
        ));
    }

    let new_config = Config {
        mcp_servers: new_mcp,
        agents: old_config.agents.clone(),
        llm: old_config.llm.clone(),
        system: old_config.system.clone(),
        tools: old_config.tools.clone(),
        tool_policies: old_config.tool_policies.clone(),
        memory: old_config.memory.clone(),
    };
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;

    let new_config = std::sync::Arc::new(new_config);
    state.apply_config(new_config, state.llm_router());

    Ok(Json(json!({ "success": true, "deleted": name })))
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

    #[test]
    fn setup_selects_the_image_for_the_native_product() {
        let setup: AgentSetup = serde_json::from_value(json!({
            "name": "reviewer",
            "runner_kind": "claude"
        }))
        .unwrap();
        let runner = runner_from_setup(&setup);
        assert_eq!(runner.kind, "claude");
        assert_eq!(
            runner.image,
            "ghcr.io/xpressai/xpressclaw-runner-claude:latest"
        );
    }

    #[tokio::test]
    async fn add_session_returns_the_logical_session_id() {
        let config_path = std::env::temp_dir().join("test-xpressclaw-add-session.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-session")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "Website maintainer",
                            "runner_kind": "codex",
                            "runner_workspace": "/tmp"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response.into_body()).await;
        assert_eq!(body["session"], "website-maintainer");
        assert!(body["session_id"].as_str().is_some_and(|id| !id.is_empty()));

        let saved = Config::load(&config_path).unwrap();
        assert_eq!(saved.agents.len(), 1);
        assert_eq!(saved.agents[0].runner.workspace.as_deref(), Some("/tmp"));
        let _ = std::fs::remove_file(&config_path);
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
        assert!(body["working_directory"].as_str().is_some());
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
        assert_eq!(config.agents[0].name, "atlas");
        // The wizard's `provider: local` should land on each agent's
        // per-agent llm config, not on a global field.
        assert_eq!(
            config.agents[0]
                .llm
                .as_ref()
                .and_then(|l| l.provider.as_deref()),
            Some("local")
        );

        // Native products own their agent/tool loop; setup must not inject
        // the retired xpressclaw agent-layer skills or MCP servers.
        assert!(config.agents[0].skills.is_empty());
        assert!(config.mcp_servers.is_empty());

        // Cleanup
        let _ = std::fs::remove_file(config_path);
    }

    /// Verify the wizard writes a valid YAML config that round-trips through
    /// Config::load and that native setup does not revive preset agent-layer
    /// MCP servers when adding another session.
    #[tokio::test]
    async fn test_wizard_writes_valid_config_with_mcp_servers() {
        // Use a unique temp path to avoid collisions with other tests.
        let config_path = std::env::temp_dir().join("test-xpressclaw-wizard-mcp.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new()
            .nest("/setup", routes())
            .with_state(state.clone());

        // ── Step 1: full setup with researcher preset ──
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "llm": {
                                "provider": "openai",
                                "api_key": "sk-test"
                            },
                            "agents": [
                                {
                                    "name": "researcher",
                                    "preset": "researcher",
                                    "tools": ["filesystem", "shell", "memory", "websearch"]
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

        // Load and validate the written config
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].name, "researcher");
        assert!(
            config.agents[0].tools.contains(&"websearch".to_string()),
            "agent should have websearch tool"
        );
        assert!(config.mcp_servers.is_empty());

        // Verify the YAML round-trips: save it again, reload, still valid
        let roundtrip_path = std::env::temp_dir().join("test-xpressclaw-wizard-roundtrip.yaml");
        config.save(&roundtrip_path).unwrap();
        let reloaded = Config::load(&roundtrip_path).unwrap();
        assert_eq!(reloaded.agents[0].name, "researcher");
        assert!(reloaded.mcp_servers.is_empty());
        let _ = std::fs::remove_file(&roundtrip_path);

        // ── Step 2: add another native session ──
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-session")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "developer",
                            "preset": "developer",
                            "tools": ["filesystem", "shell", "git", "memory"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        // Reload config — both native sessions should be preserved.
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents.len(), 2);
        let agent_names: Vec<&str> = config.agents.iter().map(|a| a.name.as_str()).collect();
        assert!(agent_names.contains(&"researcher"));
        assert!(agent_names.contains(&"developer"));

        assert!(config.mcp_servers.is_empty());

        // Cleanup
        let _ = std::fs::remove_file(&config_path);
    }

    /// Explicit connector configuration is still preserved.
    #[tokio::test]
    async fn test_frontend_mcp_servers_override_preset_defaults() {
        let config_path = std::env::temp_dir().join("test-xpressclaw-wizard-override.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        // Frontend sends custom websearch config that should override preset default
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "llm": { "provider": "local" },
                            "agents": [{
                                "name": "researcher",
                                "preset": "researcher",
                                "tools": ["filesystem", "shell", "memory", "websearch"]
                            }],
                            "mcp_servers": {
                                "websearch": {
                                    "type": "stdio",
                                    "command": "npx",
                                    "args": ["-y", "duckduckgo-mcp-server"],
                                    "env": { "SEARCH_LANG": "en" }
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let config = Config::load(&config_path).unwrap();
        let ws_cfg = config
            .mcp_servers
            .get("websearch")
            .expect("websearch MCP server missing");
        assert_eq!(
            ws_cfg.env.get("SEARCH_LANG").map(|s| s.as_str()),
            Some("en"),
            "frontend env overrides should be preserved"
        );

        let _ = std::fs::remove_file(&config_path);
    }

    /// The legacy add-agent alias should not inject preset MCP servers.
    #[tokio::test]
    async fn test_add_agent_frontend_mcp_overrides() {
        let config_path = std::env::temp_dir().join("test-xpressclaw-wizard-add-override.yaml");
        let _ = std::fs::remove_file(&config_path);

        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(config, db, None, config_path.clone(), false);
        let app = Router::new().nest("/setup", routes()).with_state(state);

        // Initial setup with assistant (no extra MCP servers)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/complete")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "llm": { "provider": "local" },
                            "agents": [{ "name": "assistant", "preset": "assistant" }]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Add researcher without explicit connectors.
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-agent")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "researcher",
                            "preset": "researcher",
                            "tools": ["filesystem", "shell", "memory", "websearch"],
                            "mcp_servers": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents.len(), 2);

        assert!(config.mcp_servers.is_empty());

        let _ = std::fs::remove_file(&config_path);
    }
}
