use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::{AgentRecord, AgentRegistry};
use xpressclaw_core::config::{
    AgentConfig, AgentLlmConfig, BudgetConfig, HooksConfig, NativeRunnerConfig, RateLimitConfig,
    WakeOnConfig,
};
use xpressclaw_core::sessions::LogicalSession;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    // Reserved for future use (image override, etc.)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_agents))
        .route("/{id}", get(get_agent).delete(delete_agent))
        .route("/{id}/config", axum::routing::patch(update_agent_config))
        .route("/{id}/start", axum::routing::post(start_agent))
        .route("/{id}/stop", axum::routing::post(stop_agent))
        .route("/{id}/logs", get(raw_logs_removed))
}

/// Build the legacy `/agents` compatibility response from a logical session.
fn agent_json(
    record: &AgentRecord,
    config: &xpressclaw_core::config::Config,
    session: Option<&LogicalSession>,
) -> Value {
    let agent_cfg = config.agents.iter().find(|a| a.name == record.name);
    let status = session.map(|s| s.status.as_str()).unwrap_or("idle");
    json!({
        "id": record.id,
        "name": record.name,
        "title": agent_cfg.map(|config| config.context_label()).unwrap_or_else(|| record.name.clone()),
        "backend": record.backend,
        "status": status,
        "desired_status": "available",
        "observed_status": "native",
        "container_id": Value::Null,
        "created_at": record.created_at,
        "started_at": record.started_at,
        "stopped_at": record.stopped_at,
        "error_message": record.error_message,
        "restart_count": record.restart_count,
        "config": agent_cfg.map(|c| json!({
            // For backward compat with frontend code that reads `model` at
            // the top level — same value as `llm.model`.
            "model": c.effective_model(),
            "llm": c.llm,
            "runner": c.runner,
            "tools": c.tools,
            "skills": c.skills,
            "volumes": c.volumes,
            "budget": c.budget,
            "rate_limit": c.rate_limit,
            "wake_on": c.wake_on,
            "idle_prompt": c.idle_prompt,
        })),
    })
}

async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let agents = registry.list().map_err(internal_error)?;
    let config = state.config();

    let sessions = xpressclaw_core::sessions::SessionManager::new(state.db.clone());
    let mut result = Vec::new();
    for a in &agents {
        let title = config
            .agents
            .iter()
            .find(|cfg| cfg.name == a.name)
            .map(|cfg| cfg.context_label())
            .unwrap_or_else(|| a.name.clone());
        let session = sessions.ensure(&a.id, Some(&title)).ok();
        result.push(agent_json(a, &config, session.as_ref()));
    }
    Ok(Json(json!(result)))
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
    let config = state.config();
    let title = config
        .agents
        .iter()
        .find(|agent| agent.name == record.name)
        .map(|agent| agent.context_label())
        .unwrap_or_else(|| record.name.clone());
    let session = xpressclaw_core::sessions::SessionManager::new(state.db.clone())
        .ensure(&record.id, Some(&title))
        .ok();
    Ok(Json(agent_json(&record, &config, session.as_ref())))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;
    let sessions = xpressclaw_core::sessions::SessionManager::new(state.db.clone());
    let attempts = sessions.list_attempts(&id, None, 1_000).unwrap_or_default();
    if let Some(docker) = state.docker().await {
        for attempt in attempts.iter().filter(|attempt| {
            !matches!(
                attempt.status.as_str(),
                "completed" | "failed" | "cancelled"
            )
        }) {
            let _ = docker.stop(&format!("attempt-{}", attempt.id)).await;
        }
        // Clean up a legacy long-running container from pre-ADR-025 installs.
        let _ = docker.stop(&id).await;
    }
    registry.delete(&id).map_err(internal_error)?;
    sessions.delete(&id).map_err(internal_error)?;

    // Remove from YAML config
    let old_config = state.config();
    let new_agents: Vec<_> = old_config
        .agents
        .iter()
        .filter(|a| a.name != id)
        .cloned()
        .collect();
    let new_config = xpressclaw_core::config::Config {
        agents: new_agents,
        llm: old_config.llm.clone(),
        mcp_servers: old_config.mcp_servers.clone(),
        system: old_config.system.clone(),
        tools: old_config.tools.clone(),
        tool_policies: old_config.tool_policies.clone(),
        memory: old_config.memory.clone(),
    };
    let _ = new_config.save(&state.config_path);
    let new_config = std::sync::Arc::new(new_config);
    state.apply_config(new_config, state.llm_router());

    Ok(StatusCode::NO_CONTENT)
}

/// Compatibility endpoint. Logical sessions are always available; worker
/// containers are created only after work is queued.
async fn start_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _body: Option<Json<StartRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    // Validate agent exists
    registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    let config = state.config();
    let record = registry.get(&id).map_err(internal_error)?;
    let session = xpressclaw_core::sessions::SessionManager::new(state.db.clone())
        .ensure(&record.id, Some(&record.name))
        .ok();
    Ok(Json(agent_json(&record, &config, session.as_ref())))
}

/// Compatibility endpoint. There is no persistent agent process to stop;
/// individual attempts are cancelled through the sessions API.
async fn stop_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let _record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    let config = state.config();
    let record = registry.get(&id).map_err(internal_error)?;
    let session = xpressclaw_core::sessions::SessionManager::new(state.db.clone())
        .ensure(&record.id, Some(&record.name))
        .ok();
    Ok(Json(agent_json(&record, &config, session.as_ref())))
}

#[derive(Debug, Deserialize)]
struct UpdateAgentConfigRequest {
    model: Option<String>,
    llm: Option<AgentLlmConfig>,
    runner: Option<NativeRunnerConfig>,
    tools: Option<Vec<String>>,
    skills: Option<Vec<String>>,
    volumes: Option<Vec<String>>,
    budget: Option<BudgetConfig>,
    rate_limit: Option<RateLimitConfig>,
    wake_on: Option<Vec<WakeOnConfig>>,
    hooks: Option<HooksConfig>,
    idle_prompt: Option<String>,
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

    // Model lives on llm.model now. If the request supplies a top-level
    // `model`, write it into llm.model — creating the AgentLlmConfig if
    // missing, so the field never gets silently dropped.
    if let Some(model) = req.model {
        let normalized = if model.is_empty() { None } else { Some(model) };
        let llm = agent.llm.get_or_insert_with(AgentLlmConfig::default);
        llm.model = normalized;
        // Drop the legacy top-level field if it was populated by an old YAML.
        agent.model = None;
    }
    if let Some(llm) = req.llm {
        // Empty provider means clear the per-agent config entirely (the agent
        // will then have no binding and the router will return a clear error
        // rather than silently routing somewhere).
        if llm.provider.as_deref().is_some_and(|p| !p.is_empty()) {
            agent.llm = Some(llm);
        } else {
            agent.llm = None;
        }
    }
    if let Some(runner) = req.runner {
        agent.runner = runner;
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
    if let Some(skills) = req.skills {
        agent.skills = skills;
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
    if let Some(idle_prompt) = req.idle_prompt {
        agent.idle_prompt = if idle_prompt.is_empty() {
            None
        } else {
            Some(idle_prompt)
        };
    }

    let needs_restart = false;

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

    // Rebuild the LLM router so changes to the agent's llm config take
    // effect immediately. Reusing the old router would silently keep the
    // pre-edit binding live until the next server restart.
    let new_config = std::sync::Arc::new(new_config);
    let new_router = std::sync::Arc::new(
        xpressclaw_core::llm::router::LlmRouter::build_from_config(&new_config),
    );
    state.apply_config(new_config.clone(), Some(new_router));

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
            "title": updated.context_label(),
            "backend": updated.backend,
            "model": updated.effective_model(),
            "llm": updated.llm.as_ref().map(|l| json!({
                "provider": l.provider,
                "model": l.model,
                "has_api_key": l.api_key.is_some(),
                "base_url": l.base_url,
            })),
            "runner": updated.runner,
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
            "idle_prompt": updated.idle_prompt,
        },
        "needs_restart": needs_restart,
    })))
}

async fn raw_logs_removed() -> (StatusCode, Json<Value>) {
    (
        StatusCode::GONE,
        Json(json!({
            "error": "raw terminal logs were replaced by structured session events and artifacts"
        })),
    )
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

    fn test_app() -> (Router, Arc<Database>) {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db.clone(),
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );

        (
            Router::new().nest("/agents", routes()).with_state(state),
            db,
        )
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_list_agents() {
        let (app, db) = test_app();
        let registry = AgentRegistry::new(db);
        registry.ensure("atlas", "generic").unwrap();

        let resp = app
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
        assert!(!body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_agent() {
        let (app, db) = test_app();
        let registry = AgentRegistry::new(db);
        registry.ensure("atlas", "generic").unwrap();

        let resp = app
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
        assert_eq!(body["status"], "idle");
    }

    #[tokio::test]
    async fn test_get_not_found() {
        let (app, _) = test_app();

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
        let (app, db) = test_app();
        let registry = AgentRegistry::new(db);
        registry.ensure("atlas", "generic").unwrap();

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

        let resp = app
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
        // Create a config with a test session
        let mut config = Config::load_default().unwrap();
        config.agents.push(AgentConfig {
            name: "atlas".to_string(),
            backend: "generic".to_string(),
            ..Default::default()
        });
        config.save(&config_path).unwrap();
        let config = Arc::new(config);
        // Register agent in DB
        let registry = AgentRegistry::new(db.clone());
        registry.ensure("atlas", "generic").unwrap();
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
        let wake_on = body["agent"]["wake_on"].as_array().unwrap();
        assert_eq!(wake_on.len(), 2);
        assert_eq!(wake_on[0]["schedule"], "every 30 minutes");
        assert_eq!(wake_on[1]["event"], "user.message");

        let hooks = &body["agent"]["hooks"];
        assert!(hooks["before_message"].as_array().unwrap().is_empty());
        assert!(hooks["after_message"].as_array().unwrap().is_empty());

        // Verify YAML persistence
        let config = Config::load(&config_path).unwrap();
        let agent = config.agents.iter().find(|a| a.name == "atlas").unwrap();
        assert_eq!(agent.wake_on.len(), 2);
        assert!(agent.hooks.before_message.is_empty());
        assert!(agent.hooks.after_message.is_empty());

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

        // Second: update only mounts — budget should be preserved
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/agents/atlas/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "volumes": ["/tmp:/tmp:ro"] }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["agent"]["volumes"][0], "/tmp:/tmp:ro");
        // Budget should still be there
        assert_eq!(body["agent"]["budget"]["daily"], "$5.00");

        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn test_stop_already_stopped() {
        let (app, db) = test_app();
        let registry = AgentRegistry::new(db);
        registry.ensure("atlas", "generic").unwrap();

        // Stop already-stopped agent — idempotent, returns 200
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

        assert_eq!(resp.status(), StatusCode::OK);
    }
}
