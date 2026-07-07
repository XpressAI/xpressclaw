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
    default_mcp_servers, AgentConfig, AgentLlmConfig, Config, LlmConfig, McpServerConfig,
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
        .route("/check-android-sdk", get(check_android_sdk))
        .route("/start-android", post(start_android))
        .route("/android-target", get(get_android_target))
        .route("/android-target", post(set_android_target))
        .route("/recommend-model", get(recommend_model))
        .route("/validate-key", post(validate_key))
        .route("/presets", get(get_presets))
        .route("/complete", post(complete_setup))
        .route("/add-agent", post(add_agent))
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
    Json(json!(info))
}

/// Check if Ollama is running and list models.
async fn check_ollama() -> Json<Value> {
    let info = detect_ollama().await;
    Json(json!(info))
}

/// Emulator lifecycle status for the managed-emulator path. Same
/// `{available, installed, can_start}` shape as `check_docker` (here: a running
/// emulator, an installed+AVD-ready SDK, and whether we can spawn one), plus the
/// detailed SDK breakdown the `/android` page uses to know which AVD to boot.
async fn check_android_sdk(State(state): State<AppState>) -> Json<Value> {
    #[cfg(feature = "android")]
    {
        use xpressclaw_core::android::sdk;
        // Both sdk::detect() (it shells out to `emulator -list-avds`/`-accel-check`)
        // and the device probe block, so run them off the async runtime. Probe the
        // SAME device the control plane drives (resolve_target), not a hardcoded
        // emulator-5554 — otherwise this lifecycle check and /v1/android/status can
        // disagree about which device they manage, e.g. offering "Start emulator"
        // for a BYO tcp device that's merely offline. Only the managed emulator can
        // actually be spawned.
        let config = state.config();
        match tokio::task::spawn_blocking(move || {
            let status = sdk::detect();
            let target = crate::routes::android::resolve_target(&config);
            let is_managed = target.is_managed_emulator();
            (status, target.booted(), is_managed)
        })
        .await
        {
            Ok((status, available, is_managed)) => {
                let installed = status.ready();
                Json(json!({
                    "available": available,                              // the target device is up & booted
                    "installed": installed,                              // SDK + emulator + image + AVD
                    "can_start": installed && !available && is_managed,  // safe to spawn the managed emulator
                    "sdk": status,
                }))
            }
            Err(_) => Json(json!({ "available": false, "installed": false, "can_start": false })),
        }
    }
    #[cfg(not(feature = "android"))]
    {
        let _ = state;
        Json(json!({ "available": false, "installed": false, "can_start": false }))
    }
}

#[derive(Deserialize)]
struct StartAndroidRequest {
    /// AVD to boot; defaults to the first available AVD if omitted.
    #[serde(default)]
    avd: Option<String>,
}

/// Spawn a managed emulator. Mirrors `start_docker`. macOS/Windows/Linux all
/// launch the SDK `emulator` binary detached.
async fn start_android(
    Json(req): Json<StartAndroidRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    #[cfg(feature = "android")]
    {
        use xpressclaw_core::android::{emulator, sdk};
        let avd = match req.avd {
            Some(a) if !a.trim().is_empty() => a,
            _ => sdk::detect().avds.into_iter().next().ok_or_else(|| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "no AVD available — create one first" })),
                )
            })?,
        };
        emulator::start(&avd).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?;
        Ok(Json(json!({ "started": true, "avd": avd })))
    }
    #[cfg(not(feature = "android"))]
    {
        let _ = req;
        Err((
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({ "error": "android feature not enabled" })),
        ))
    }
}

/// The configured android device target (top-level `android` config section).
async fn get_android_target(State(state): State<AppState>) -> Json<Value> {
    let android = &state.config().android;
    Json(json!({ "serial": android.serial, "tcp": android.tcp }))
}

#[derive(Deserialize)]
struct AndroidTargetRequest {
    serial: Option<String>,
    tcp: Option<String>,
}

/// Persist the android device target. Empty strings clear a field; `tcp`
/// overrides `serial` at resolve time (see routes::android::resolve_target).
async fn set_android_target(
    State(state): State<AppState>,
    Json(req): Json<AndroidTargetRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let normalize = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let tcp = normalize(req.tcp);
    if let Some(addr) = &tcp {
        if addr.parse::<std::net::SocketAddr>().is_err() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("'{addr}' is not a valid host:port address") })),
            ));
        }
    }

    let mut new_config = (*state.config()).clone();
    new_config.android = xpressclaw_core::config::AndroidConfig {
        serial: normalize(req.serial),
        tcp,
    };
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;

    let android = new_config.android.clone();
    state.apply_config(std::sync::Arc::new(new_config), state.llm_router());
    Ok(Json(json!({ "success": true, "serial": android.serial, "tcp": android.tcp })))
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
    /// Ollama-tag-style model name (e.g. "qwen3.5:4b") used when
    /// `provider == "ollama"`. The reconciler will pull it from the agent's
    /// Ollama instance in the background after setup completes.
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
    model: Option<String>,
    tools: Option<Vec<String>>,
    volumes: Option<Vec<String>>,
    /// MCP servers to merge into global config (used by add-agent flow).
    #[serde(default)]
    mcp_servers: std::collections::HashMap<String, McpServerConfig>,
}

/// Save the setup configuration and mark setup as complete.
async fn complete_setup(
    State(state): State<AppState>,
    Json(req): Json<CompleteSetupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Global LLM config no longer holds provider/model/key — those moved
    // onto each agent. Only `custom_pricing` lives here.
    let llm = LlmConfig::default();

    // Agents
    let presets = builtin_presets();
    let agents = if req.agents.is_empty() {
        vec![AgentConfig {
            name: "atlas".to_string(),
            backend: "claude-sdk".to_string(),
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

                let mut tools = a
                    .tools
                    .clone()
                    .or(preset.map(|p| p.default_tools.iter().map(|s| s.to_string()).collect()))
                    .unwrap_or_default();
                // Shell + filesystem are always included
                for default_tool in ["filesystem", "shell"] {
                    if !tools.iter().any(|t| t == default_tool) {
                        tools.insert(0, default_tool.to_string());
                    }
                }

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
                        .backend
                        .clone()
                        .or(preset.map(|p| p.backend.to_string()))
                        .unwrap_or("claude-sdk".to_string()),
                    role: a
                        .role
                        .clone()
                        .or(preset.map(|p| p.role.to_string()))
                        .unwrap_or_default(),
                    llm: agent_llm,
                    tools,
                    skills: vec![
                        "memory-system".to_string(),
                        "task-management".to_string(),
                        "build-app".to_string(),
                    ],
                    volumes: a.volumes.clone().unwrap_or_default(),
                    ..Default::default()
                }
            })
            .collect()
    };

    // Merge MCP servers: built-in defaults + preset + frontend overrides.
    let mut mcp_servers = req.mcp_servers;
    // Built-in defaults (tasks, memory, skills, apps, shell, filesystem)
    for (name, server) in default_mcp_servers() {
        mcp_servers.entry(name).or_insert(server);
    }
    // Preset-specific servers
    for agent_setup in &req.agents {
        if let Some(preset) = agent_setup
            .preset
            .as_deref()
            .and_then(|id| presets.iter().find(|p| p.id == id))
        {
            for (name, server) in &preset.default_mcp_servers {
                if !mcp_servers.contains_key(name) {
                    mcp_servers.insert(name.clone(), server.clone());
                }
            }
        }
    }

    let mut config = Config {
        llm,
        agents,
        mcp_servers,
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
    let existing_agents = registry.list().unwrap_or_default();
    let new_agent_names: std::collections::HashSet<&str> =
        config.agents.iter().map(|a| a.name.as_str()).collect();
    for existing in &existing_agents {
        if !new_agent_names.contains(existing.name.as_str()) {
            info!(name = existing.name, "removing agent not in new config");
            let _ = registry.delete(&existing.id);
        }
    }
    for agent_config in &config.agents {
        match registry.ensure(&agent_config.name, &agent_config.backend) {
            Ok(record) => {
                info!(name = record.name, backend = record.backend, "synced agent");
                // Auto-start all agents after setup
                let _ = registry.set_desired_status(
                    &record.id,
                    &xpressclaw_core::agents::state::DesiredStatus::Running,
                );
            }
            Err(e) => warn!(name = agent_config.name, error = %e, "failed to sync agent"),
        }
    }

    // Build LLM router from the new config
    let llm_router = LlmRouter::build_from_config(&config);
    state.apply_config(config, Some(Arc::new(llm_router)));
    info!("configuration applied — setup complete");

    // Any agent using provider=ollama will have its model pulled by the
    // background reconciler (see agents::reconciler::reconcile_models). No
    // synchronous download here.

    Ok(Json(json!({
        "success": true,
        "downloading": false,
        "config_path": state.config_path.display().to_string()
    })))
}

/// Add a new agent to the existing configuration without replacing other agents.
/// Used by the "+ Add Agent" flow (mode=add-agent) in the wizard.
async fn add_agent(
    State(state): State<AppState>,
    Json(req): Json<AgentSetup>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let presets = builtin_presets();
    let preset = req
        .preset
        .as_deref()
        .and_then(|id| presets.iter().find(|p| p.id == id));

    let mut tools = req
        .tools
        .clone()
        .or(preset.map(|p| p.default_tools.iter().map(|s| s.to_string()).collect()))
        .unwrap_or_default();
    for default_tool in ["filesystem", "shell"] {
        if !tools.iter().any(|t| t == default_tool) {
            tools.insert(0, default_tool.to_string());
        }
    }

    // New agents get a sensible default LLM config that points at local
    // Ollama. The user edits the agent afterwards to switch provider/model
    // or add an API key. We deliberately do NOT inherit any "global" provider
    // or key from existing agents — that's the bug this rewrite fixes.
    let agent_llm = Some(AgentLlmConfig {
        provider: Some("ollama".into()),
        model: req.model.clone().or_else(|| Some("qwen3.5:latest".into())),
        api_key: None,
        base_url: Some("http://localhost:11434".into()),
    });

    // Default skills for new agents
    let default_skills = vec![
        "memory-system".to_string(),
        "task-management".to_string(),
        "build-app".to_string(),
    ];

    // Slugify the name and ensure uniqueness
    let old_config = state.config();
    let existing_ids: Vec<&str> = old_config.agents.iter().map(|a| a.name.as_str()).collect();
    let agent_id = xpressclaw_core::config::unique_agent_id(&req.name, &existing_ids);

    let agent_config = AgentConfig {
        name: agent_id.clone(),
        display_name: Some(req.name.clone()),
        backend: req
            .backend
            .clone()
            .or(preset.map(|p| p.backend.to_string()))
            .unwrap_or("claude-sdk".to_string()),
        role: req
            .role
            .clone()
            .or(preset.map(|p| p.role.to_string()))
            .unwrap_or_default(),
        llm: agent_llm,
        tools,
        skills: default_skills,
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

    // Merge MCP servers: built-in defaults + preset + existing + frontend overrides.
    let mut new_mcp = old_config.mcp_servers.clone();
    // Add built-in defaults (tasks, memory, apps, skills, shell, filesystem)
    for (name, server) in default_mcp_servers() {
        new_mcp.entry(name).or_insert(server);
    }
    // Add preset-specific MCP servers
    if let Some(preset) = preset {
        for (name, server) in &preset.default_mcp_servers {
            if !new_mcp.contains_key(name) {
                new_mcp.insert(name.clone(), server.clone());
            }
        }
    }
    // Frontend-provided MCP servers override defaults.
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
    info!(name = agent_config.name, "added agent to configuration");

    // Register in DB and auto-start
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry
        .ensure(&agent_config.name, &agent_config.backend)
        .map_err(internal_error)?;
    let _ = registry.set_desired_status(
        &record.id,
        &xpressclaw_core::agents::state::DesiredStatus::Running,
    );

    // Rebuild the router so the new agent has a binding. The previous
    // implementation reused the existing router, which silently meant the
    // newly-added agent had no provider mapping until the server restarted.
    let new_config = std::sync::Arc::new(new_config);
    let new_router = Arc::new(LlmRouter::build_from_config(&new_config));
    state.apply_config(new_config, Some(new_router));

    Ok(Json(json!({
        "success": true,
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
        android: old_config.android.clone(),
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
        android: old_config.android.clone(),
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

        // Verify agent has default skills
        assert!(
            config.agents[0]
                .skills
                .contains(&"memory-system".to_string()),
            "agent should have memory-system skill, got: {:?}",
            config.agents[0].skills
        );
        assert!(
            config.agents[0]
                .skills
                .contains(&"task-management".to_string()),
            "agent should have task-management skill"
        );
        assert!(
            config.agents[0].skills.contains(&"build-app".to_string()),
            "agent should have build-app skill"
        );

        // Verify default MCP servers are present
        assert!(
            config.mcp_servers.contains_key("xpressclaw"),
            "should have xpressclaw MCP server (unified tasks/memory/skills/apps), got: {:?}",
            config.mcp_servers.keys().collect::<Vec<_>>()
        );
        assert!(
            config.mcp_servers.contains_key("shell"),
            "should have shell MCP server"
        );
        assert!(
            config.mcp_servers.contains_key("filesystem"),
            "should have filesystem MCP server"
        );

        // Cleanup
        let _ = std::fs::remove_file(config_path);
    }

    /// Verify the wizard writes a valid YAML config that round-trips through
    /// Config::load, that preset MCP servers are merged, and that add-agent
    /// also merges its preset's MCP servers.
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
        // Preset default_mcp_servers should have been merged
        assert!(
            config.mcp_servers.contains_key("websearch"),
            "websearch MCP server should be configured from preset"
        );
        let ws_cfg = &config.mcp_servers["websearch"];
        assert_eq!(ws_cfg.server_type, "stdio");
        assert_eq!(ws_cfg.command.as_deref(), Some("npx"));

        // Verify the YAML round-trips: save it again, reload, still valid
        let roundtrip_path = std::env::temp_dir().join("test-xpressclaw-wizard-roundtrip.yaml");
        config.save(&roundtrip_path).unwrap();
        let reloaded = Config::load(&roundtrip_path).unwrap();
        assert_eq!(reloaded.agents[0].name, "researcher");
        assert!(reloaded.mcp_servers.contains_key("websearch"));
        let _ = std::fs::remove_file(&roundtrip_path);

        // ── Step 2: add developer agent via add-agent ──
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/setup/add-agent")
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

        // Reload config — should now have both agents and their MCP servers
        let config = Config::load(&config_path).unwrap();
        assert_eq!(config.agents.len(), 2);
        let agent_names: Vec<&str> = config.agents.iter().map(|a| a.name.as_str()).collect();
        assert!(agent_names.contains(&"researcher"));
        assert!(agent_names.contains(&"developer"));

        // Researcher's websearch should still be there
        assert!(
            config.mcp_servers.contains_key("websearch"),
            "websearch MCP server should be preserved"
        );

        // Cleanup
        let _ = std::fs::remove_file(&config_path);
    }

    /// Frontend-provided MCP servers should override preset defaults,
    /// and explicit env vars (like URL filters) should be preserved.
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
        // Frontend's explicit config should win over preset default
        assert_eq!(
            ws_cfg.env.get("SEARCH_LANG").map(|s| s.as_str()),
            Some("en"),
            "frontend env overrides should be preserved"
        );

        let _ = std::fs::remove_file(&config_path);
    }

    /// add-agent with researcher preset should merge its MCP servers.
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

        // Add researcher — preset's websearch MCP server should be merged
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

        // Researcher preset's websearch MCP server should be present
        let ws_cfg = config
            .mcp_servers
            .get("websearch")
            .expect("websearch MCP server missing from researcher preset");
        assert_eq!(ws_cfg.command.as_deref(), Some("npx"),);

        let _ = std::fs::remove_file(&config_path);
    }
}
