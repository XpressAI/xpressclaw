use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::{AgentRegistry, RegisterAgent};
use xpressclaw_core::agents::state::AgentStatus;
use xpressclaw_core::config::{
    default_mcp_servers, AgentConfig, AgentLlmConfig, BudgetConfig, HooksConfig, McpServerConfig,
    RateLimitConfig, WakeOnConfig,
};
use xpressclaw_core::docker::images::build_container_spec;
use xpressclaw_core::docker::manager::{DockerManager, VolumeMount};

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    /// Override the harness image (optional).
    pub image: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_agents).post(register_agent))
        .route(
            "/{id}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/{id}/config", axum::routing::patch(update_agent_config))
        .route("/{id}/start", axum::routing::post(start_agent))
        .route("/{id}/stop", axum::routing::post(stop_agent))
}

async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let agents = registry.list().map_err(internal_error)?;
    Ok(Json(json!(agents)))
}

async fn register_agent(
    State(state): State<AppState>,
    Json(req): Json<RegisterAgent>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.register(&req).map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(record))))
}

async fn get_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(record)))
}

async fn update_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<RegisterAgent>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    // Ensure agent exists first
    registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    let record = registry.register(&req).map_err(internal_error)?;
    Ok(Json(json!(record)))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    // Stop container if running
    if let Ok(record) = registry.get(&id) {
        if record.status == "running" {
            if let Ok(docker) = DockerManager::connect().await {
                let _ = docker.stop(&id).await;
            }
        }
    }
    registry.delete(&id).map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn start_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Option<Json<StartRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    if record.status == "running" {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": format!("agent '{}' is already running", id) })),
        ));
    }

    // Mark as starting
    registry
        .update_status(&id, &AgentStatus::Starting, None)
        .map_err(internal_error)?;

    let config = state.config();
    let isolation = config.system.isolation.as_str();

    if isolation == "none" {
        // Containerless mode: just mark the agent as running.
        // The conversation handler calls the LLM router directly.
        let record = registry
            .update_status(&id, &AgentStatus::Running, None)
            .map_err(internal_error)?;
        return Ok(Json(json!(record)));
    }

    // Docker isolation mode
    let docker = DockerManager::connect().await.map_err(|e| {
        let _ = registry.update_status(&id, &AgentStatus::Error(e.to_string()), None);
        internal_error(e)
    })?;

    let server_port = std::env::var("XPRESSCLAW_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8935);

    // Build container spec from agent config (handles env vars, API keys, volumes)
    let agent_cfg = config.agents.iter().find(|a| a.name == record.name);
    let mut spec = if let Some(cfg) = agent_cfg {
        let mut s = build_container_spec(
            cfg,
            server_port,
            config.llm.anthropic_api_key.as_deref(),
            config.llm.openai_api_key.as_deref(),
            config.llm.openai_base_url.as_deref(),
        );
        // Override image if requested
        if let Some(ref b) = body {
            if let Some(ref img) = b.image {
                s.image = img.clone();
            }
        }
        s
    } else {
        // Fallback for agents not in YAML config
        use xpressclaw_core::docker::images::image_for_backend;
        let image = body
            .and_then(|b| b.image.clone())
            .unwrap_or_else(|| image_for_backend(&record.backend).to_string());
        let mut s = xpressclaw_core::docker::manager::ContainerSpec {
            image,
            ..Default::default()
        };
        let server_base = format!("http://host.docker.internal:{server_port}");
        s.environment.push(format!("AGENT_ID={id}"));
        s.environment.push(format!("AGENT_NAME={}", record.name));
        s.environment
            .push(format!("AGENT_BACKEND={}", record.backend));
        s.environment.push(format!("LLM_BASE_URL={server_base}/v1"));
        s.environment
            .push(format!("OPENAI_BASE_URL={server_base}/v1"));
        s.environment
            .push("OPENAI_API_KEY=sk-xpressclaw".to_string());
        s.environment
            .push(format!("ANTHROPIC_BASE_URL={server_base}"));
        s.environment
            .push(format!("ANTHROPIC_API_KEY=sk-ant-{}", record.name));
        s
    };

    // Always pass the agent config JSON
    spec.environment
        .push(format!("AGENT_CONFIG={}", record.config));

    // Filter MCP servers: always-on defaults + agent's tools matching global MCP servers
    let filtered_mcp = filter_mcp_for_agent(agent_cfg, &config.mcp_servers);
    if !filtered_mcp.is_empty() {
        if let Ok(mcp_json) = serde_json::to_string(&filtered_mcp) {
            spec.environment.push(format!("MCP_SERVERS={mcp_json}"));
        }
    }

    // Pass agent tools list
    if let Some(cfg) = agent_cfg {
        if !cfg.tools.is_empty() {
            if let Ok(tools_json) = serde_json::to_string(&cfg.tools) {
                spec.environment.push(format!("AGENT_TOOLS={tools_json}"));
            }
        }
    }

    // If agent has no per-agent volumes, fall back to global workspace dir
    if spec.volumes.is_empty() {
        let workspace_dir = config.system.workspace_dir.to_string_lossy().to_string();
        if !workspace_dir.is_empty() && workspace_dir != "/" {
            spec.volumes.push(VolumeMount {
                source: workspace_dir,
                target: "/workspace".to_string(),
                read_only: false,
            });
        }
    }

    // Mount Docker socket if any MCP server needs docker access (e.g., GitHub MCP)
    if filtered_mcp
        .values()
        .any(|s| s.command.as_deref() == Some("docker"))
    {
        spec.volumes.push(VolumeMount {
            source: "/var/run/docker.sock".to_string(),
            target: "/var/run/docker.sock".to_string(),
            read_only: false,
        });
    }

    match docker.launch(&id, &spec).await {
        Ok(info) => {
            let record = registry
                .update_status(&id, &AgentStatus::Running, Some(&info.container_id))
                .map_err(internal_error)?;
            Ok(Json(json!(record)))
        }
        Err(e) => {
            let record = registry
                .update_status(&id, &AgentStatus::Error(e.to_string()), None)
                .map_err(internal_error)?;
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": e.to_string(),
                    "agent": record,
                })),
            ))
        }
    }
}

async fn stop_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    if record.status == "stopped" {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": format!("agent '{}' is already stopped", id) })),
        ));
    }

    // Stop the container
    if let Ok(docker) = DockerManager::connect().await {
        let _ = docker.stop(&id).await;
    }

    let record = registry
        .update_status(&id, &AgentStatus::Stopped, None)
        .map_err(internal_error)?;
    Ok(Json(json!(record)))
}

#[derive(Debug, Deserialize)]
struct UpdateAgentConfigRequest {
    role: Option<String>,
    model: Option<String>,
    llm: Option<AgentLlmConfig>,
    tools: Option<Vec<String>>,
    volumes: Option<Vec<String>>,
    budget: Option<BudgetConfig>,
    rate_limit: Option<RateLimitConfig>,
    wake_on: Option<Vec<WakeOnConfig>>,
    hooks: Option<HooksConfig>,
}

/// Update an agent's configuration in the YAML config file and reload.
async fn update_agent_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAgentConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    let old_config = state.config();
    let mut new_agents = old_config.agents.clone();

    // Find or create the agent config entry
    let agent_idx = new_agents.iter().position(|a| a.name == record.name);
    let agent = if let Some(idx) = agent_idx {
        &mut new_agents[idx]
    } else {
        new_agents.push(AgentConfig {
            name: record.name.clone(),
            backend: record.backend.clone(),
            ..Default::default()
        });
        new_agents.last_mut().unwrap()
    };

    // Apply partial updates
    if let Some(role) = req.role {
        agent.role = role;
    }
    if let Some(model) = req.model {
        agent.model = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(llm) = req.llm {
        // Empty provider means clear the override
        if llm.provider.as_deref().is_some_and(|p| !p.is_empty()) {
            // Merge with existing: preserve api_key if not provided
            let existing = agent.llm.take().unwrap_or_default();
            agent.llm = Some(AgentLlmConfig {
                provider: llm.provider.or(existing.provider),
                api_key: llm.api_key.or(existing.api_key),
                base_url: llm.base_url.or(existing.base_url),
            });
        } else {
            agent.llm = None;
        }
    }
    if let Some(mut tools) = req.tools {
        // Ensure shell + filesystem are always present
        for default_tool in ["filesystem", "shell"] {
            if !tools.iter().any(|t| t == default_tool) {
                tools.insert(0, default_tool.to_string());
            }
        }
        agent.tools = tools;
    }
    if let Some(volumes) = req.volumes {
        agent.volumes = volumes;
    }
    if let Some(budget) = req.budget {
        agent.budget = Some(budget);
    }
    if let Some(rate_limit) = req.rate_limit {
        agent.rate_limit = Some(rate_limit);
    }
    if let Some(wake_on) = req.wake_on {
        agent.wake_on = wake_on;
    }
    if let Some(hooks) = req.hooks {
        agent.hooks = hooks;
    }

    let needs_restart = record.status == "running";

    // Save updated config — preserve all top-level fields
    let new_config = xpressclaw_core::config::Config {
        agents: new_agents,
        llm: old_config.llm.clone(),
        mcp_servers: old_config.mcp_servers.clone(),
        system: old_config.system.clone(),
        tools: old_config.tools.clone(),
        tool_policies: old_config.tool_policies.clone(),
        memory: old_config.memory.clone(),
    };
    new_config
        .save(&state.config_path)
        .map_err(internal_error)?;

    // Reload config into AppState (keep existing LLM router)
    let new_config = std::sync::Arc::new(new_config);
    state.apply_config(new_config.clone(), state.llm_router());

    // Find the updated agent config to return
    let updated = new_config
        .agents
        .iter()
        .find(|a| a.name == record.name)
        .cloned()
        .unwrap_or_default();

    Ok(Json(json!({
        "agent": {
            "name": updated.name,
            "backend": updated.backend,
            "role": updated.role,
            "model": updated.model,
            "llm": updated.llm.as_ref().map(|l| json!({
                "provider": l.provider,
                "api_key": l.api_key.as_ref().map(|_| "********"),
                "base_url": l.base_url,
            })),
            "tools": updated.tools,
            "volumes": updated.volumes,
            "budget": updated.budget.as_ref().map(|b| json!({
                "daily": b.daily, "monthly": b.monthly, "per_task": b.per_task,
                "on_exceeded": serde_json::to_value(&b.on_exceeded).unwrap_or(json!("pause")),
                "fallback_model": b.fallback_model,
                "warn_at_percent": b.warn_at_percent,
            })),
            "rate_limit": updated.rate_limit.as_ref().map(|r| json!({
                "requests_per_minute": r.requests_per_minute,
                "tokens_per_minute": r.tokens_per_minute,
                "concurrent_requests": r.concurrent_requests,
            })),
            "wake_on": updated.wake_on.iter().map(|w| json!({
                "schedule": w.schedule, "event": w.event, "condition": w.condition,
            })).collect::<Vec<_>>(),
            "hooks": {
                "before_message": updated.hooks.before_message,
                "after_message": updated.hooks.after_message,
            },
        },
        "needs_restart": needs_restart,
    })))
}

/// Build the set of MCP servers for an agent: always-on defaults (shell, filesystem)
/// plus any global MCP servers whose key appears in the agent's tools list.
fn filter_mcp_for_agent(
    agent_cfg: Option<&AgentConfig>,
    global_mcp: &HashMap<String, McpServerConfig>,
) -> HashMap<String, McpServerConfig> {
    let mut result = default_mcp_servers();
    if let Some(cfg) = agent_cfg {
        for tool in &cfg.tools {
            if let Some(server) = global_mcp.get(tool.as_str()) {
                result.insert(tool.clone(), server.clone());
            }
        }
    } else {
        // No per-agent config: pass all global MCP servers
        result.extend(global_mcp.clone());
    }
    result
}

fn internal_error(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

fn not_found(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
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

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db,
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );

        Router::new().nest("/agents", routes()).with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_register_and_list() {
        let app = test_app();

        // Register
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {"role": "You are a helpful assistant"}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["name"], "atlas");
        assert_eq!(body["backend"], "generic");
        assert_eq!(body["status"], "stopped");

        // List
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_get_agent() {
        let app = test_app();

        // Register
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Get
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agents/atlas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["id"], "atlas");
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/agents/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_agent() {
        let app = test_app();

        // Register
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Delete
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/agents/atlas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Verify gone
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/agents/atlas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Helper: create a test app with a real config file path for update tests.
    fn test_app_with_config() -> (Router, std::path::PathBuf) {
        let config_path = std::env::temp_dir().join(format!(
            "test-xpressclaw-agent-{}.yaml",
            uuid::Uuid::new_v4().simple()
        ));
        let db = Arc::new(Database::open_memory().unwrap());
        // Create a config with a test agent
        let mut config = Config::load_default().unwrap();
        config.agents.push(AgentConfig {
            name: "atlas".to_string(),
            backend: "generic".to_string(),
            role: "You are a test agent.".to_string(),
            ..Default::default()
        });
        config.save(&config_path).unwrap();
        let config = Arc::new(config);
        // Register agent in DB
        let registry = AgentRegistry::new(db.clone());
        registry
            .register(&RegisterAgent {
                name: "atlas".to_string(),
                backend: "generic".to_string(),
                config: json!({"role": "You are a test agent."}),
            })
            .unwrap();
        let state = AppState::new(config, db, None, config_path.clone(), true);
        let app = Router::new().nest("/agents", routes()).with_state(state);
        (app, config_path)
    }

    #[tokio::test]
    async fn test_update_config_budget() {
        let (app, config_path) = test_app_with_config();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/agents/atlas/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "budget": {
                                "daily": "$10.00",
                                "monthly": "$200.00",
                                "per_task": null,
                                "on_exceeded": "alert",
                                "fallback_model": "local",
                                "warn_at_percent": 90
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["agent"]["budget"]["daily"], "$10.00");
        assert_eq!(body["agent"]["budget"]["on_exceeded"], "alert");
        assert_eq!(body["agent"]["budget"]["warn_at_percent"], 90);

        // Verify persisted to YAML
        let config = Config::load(&config_path).unwrap();
        let agent = config.agents.iter().find(|a| a.name == "atlas").unwrap();
        let budget = agent.budget.as_ref().unwrap();
        assert_eq!(budget.daily.as_deref(), Some("$10.00"));
        assert_eq!(budget.warn_at_percent, 90);

        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn test_update_config_rate_limit() {
        let (app, config_path) = test_app_with_config();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/agents/atlas/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "rate_limit": {
                                "requests_per_minute": 30,
                                "tokens_per_minute": 50000,
                                "concurrent_requests": 2
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["agent"]["rate_limit"]["requests_per_minute"], 30);
        assert_eq!(body["agent"]["rate_limit"]["concurrent_requests"], 2);

        let config = Config::load(&config_path).unwrap();
        let agent = config.agents.iter().find(|a| a.name == "atlas").unwrap();
        let rl = agent.rate_limit.as_ref().unwrap();
        assert_eq!(rl.requests_per_minute, 30);

        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn test_update_config_wake_on_and_hooks() {
        let (app, config_path) = test_app_with_config();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/agents/atlas/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "wake_on": [
                                {"schedule": "every 30 minutes", "event": null, "condition": null},
                                {"schedule": null, "event": "user.message", "condition": null}
                            ],
                            "hooks": {
                                "before_message": ["memory_recall"],
                                "after_message": ["memory_remember"]
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let wake_on = body["agent"]["wake_on"].as_array().unwrap();
        assert_eq!(wake_on.len(), 2);
        assert_eq!(wake_on[0]["schedule"], "every 30 minutes");
        assert_eq!(wake_on[1]["event"], "user.message");

        let hooks = &body["agent"]["hooks"];
        assert_eq!(hooks["before_message"][0], "memory_recall");
        assert_eq!(hooks["after_message"][0], "memory_remember");

        // Verify YAML persistence
        let config = Config::load(&config_path).unwrap();
        let agent = config.agents.iter().find(|a| a.name == "atlas").unwrap();
        assert_eq!(agent.wake_on.len(), 2);
        assert_eq!(agent.hooks.before_message, vec!["memory_recall"]);
        assert_eq!(agent.hooks.after_message, vec!["memory_remember"]);

        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn test_update_config_preserves_unmodified_fields() {
        let (app, config_path) = test_app_with_config();

        // First: set budget
        app.clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/agents/atlas/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "budget": {
                                "daily": "$5.00",
                                "on_exceeded": "pause",
                                "fallback_model": "local",
                                "warn_at_percent": 80
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Second: update only role — budget should be preserved
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/agents/atlas/config")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "role": "Updated role." }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["agent"]["role"], "Updated role.");
        // Budget should still be there
        assert_eq!(body["agent"]["budget"]["daily"], "$5.00");

        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn test_stop_already_stopped() {
        let app = test_app();

        // Register (starts as stopped)
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "atlas",
                            "backend": "generic",
                            "config": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Stop already-stopped agent
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/agents/atlas/stop")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }
}
