use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::{AgentRecord, AgentRegistry};
use xpressclaw_core::agents::state::{DesiredStatus, ObservedStatus};
use xpressclaw_core::config::{
    AgentConfig, AgentLlmConfig, BudgetConfig, HooksConfig, RateLimitConfig, WakeOnConfig,
};


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
        .route("/{id}/logs", get(get_agent_logs))
}

/// Build a JSON response for an agent by merging YAML config and DB state.
fn agent_json(
    record: &AgentRecord,
    config: &xpressclaw_core::config::Config,
    observed: &xpressclaw_core::agents::state::ObservedStatus,
) -> Value {
    let agent_cfg = config.agents.iter().find(|a| a.name == record.name);
    let desired: xpressclaw_core::agents::state::DesiredStatus = record
        .desired_status
        .parse()
        .unwrap_or(xpressclaw_core::agents::state::DesiredStatus::Stopped);
    let status = xpressclaw_core::agents::state::compute_status(&desired, observed);
    json!({
        "id": record.id,
        "name": record.name,
        "backend": record.backend,
        "status": status,
        "desired_status": record.desired_status,
        "observed_status": observed.to_string(),
        "created_at": record.created_at,
        "started_at": record.started_at,
        "stopped_at": record.stopped_at,
        "error_message": record.error_message,
        "restart_count": record.restart_count,
        "config": agent_cfg.map(|c| json!({
            "display_name": c.display_name,
            "role_title": c.role_title,
            "responsibilities": c.responsibilities,
            "avatar": c.avatar,
            "role": c.role,
            "model": c.model,
            "llm": c.llm,
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

    let mut result = Vec::new();
    for a in &agents {
        let observed = ObservedStatus::DockerUnavailable;
        result.push(agent_json(a, &config, &observed));
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
    let observed = ObservedStatus::DockerUnavailable;
    Ok(Json(agent_json(&record, &config, &observed)))
}

async fn delete_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    // Set desired=stopped so the reconciler stops the container
    let _ = registry.set_desired_status(&id, &DesiredStatus::Stopped);
    registry.delete(&id).map_err(internal_error)?;

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

/// Start an agent: sets desired_status to 'running'.
/// The reconciler handles image pulling and container launch.
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

    registry
        .set_desired_status(&id, &DesiredStatus::Running)
        .map_err(internal_error)?;

    // Also set old status for backward compat during transition
    let _ = registry.update_status(
        &id,
        &xpressclaw_core::agents::state::AgentStatus::Starting,
        None,
    );

    let config = state.config();
    let record = registry.get(&id).map_err(internal_error)?;
    let observed = ObservedStatus::DockerUnavailable;
    Ok(Json(agent_json(&record, &config, &observed)))
}

/// Stop an agent: sets desired_status to 'stopped'.
/// The reconciler handles container shutdown.
async fn stop_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = AgentRegistry::new(state.db.clone());
    let _record = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    registry
        .set_desired_status(&id, &DesiredStatus::Stopped)
        .map_err(internal_error)?;

    // Also set old status for backward compat during transition
    let _ = registry.update_status(
        &id,
        &xpressclaw_core::agents::state::AgentStatus::Stopped,
        None,
    );

    let config = state.config();
    let record = registry.get(&id).map_err(internal_error)?;
    let observed = ObservedStatus::DockerUnavailable;
    Ok(Json(agent_json(&record, &config, &observed)))
}

#[derive(Debug, Deserialize)]
struct UpdateAgentConfigRequest {
    display_name: Option<String>,
    role_title: Option<String>,
    responsibilities: Option<String>,
    avatar: Option<String>,
    role: Option<String>,
    model: Option<String>,
    llm: Option<AgentLlmConfig>,
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

    // Apply partial updates — profile fields
    if let Some(dn) = req.display_name {
        agent.display_name = if dn.is_empty() { None } else { Some(dn) };
    }
    if let Some(rt) = req.role_title {
        agent.role_title = if rt.is_empty() { None } else { Some(rt) };
    }
    if let Some(resp) = req.responsibilities {
        agent.responsibilities = if resp.is_empty() { None } else { Some(resp) };
    }
    if let Some(av) = req.avatar {
        agent.avatar = if av.is_empty() { None } else { Some(av) };
    }
    if let Some(role) = req.role {
        agent.role = role;
    }
    if let Some(model) = req.model {
        agent.model = if model.is_empty() { None } else { Some(model) };
    }
    if let Some(llm) = req.llm {
        // Empty provider means clear the override
        if llm.provider.as_deref().is_some_and(|p| !p.is_empty()) {
            agent.llm = Some(llm);
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

    let needs_restart = record.desired_status == "running";

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
            "display_name": updated.display_name,
            "role_title": updated.role_title,
            "responsibilities": updated.responsibilities,
            "avatar": updated.avatar,
            "role": updated.role,
            "model": updated.model,
            "llm": updated.llm.as_ref().map(|l| json!({
                "provider": l.provider,
                "api_key": l.api_key,
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
            "idle_prompt": updated.idle_prompt,
        },
        "needs_restart": needs_restart,
    })))
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    tail: Option<usize>,
}

async fn get_agent_logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(_query): Query<LogsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Validate agent exists
    let registry = AgentRegistry::new(state.db.clone());
    let _ = registry.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::AgentNotFound { .. } => not_found(&e),
        _ => internal_error(e),
    })?;

    Ok(Json(json!({ "logs": "Logs not available without container runtime" })))
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
        assert_eq!(body["status"], "stopped");
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
