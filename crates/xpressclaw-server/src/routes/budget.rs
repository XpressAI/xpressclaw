use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::budget::manager::BudgetManager;
use xpressclaw_core::budget::tracker::CostTracker;

use crate::state::AppState;

#[derive(Deserialize)]
pub struct UsageParams {
    pub agent_id: Option<String>,
    pub limit: Option<i64>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(budget_summary))
        .route("/usage", get(usage_history))
        .route("/{agent_id}", get(agent_budget))
        .route("/{agent_id}/resume", axum::routing::post(resume_agent))
}

async fn budget_summary(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = BudgetManager::new(state.db.clone(), state.config());
    let summary = mgr.get_summary(None).map_err(internal_error)?;

    // Per-agent breakdown
    let agents = mgr.get_top_spenders(100).map_err(internal_error)?;
    let agent_list: Vec<Value> = agents
        .iter()
        .map(|a| {
            json!({
                "agent_id": a.agent_id,
                "daily_spent": a.daily_spent,
                "monthly_spent": a.monthly_spent,
                "total_spent": a.total_spent,
                "is_paused": a.is_paused,
            })
        })
        .collect();

    Ok(Json(json!({
        "global": {
            "daily_limit": summary.daily_limit,
            "monthly_limit": summary.monthly_limit,
            "daily_spent": summary.daily_spent,
            "monthly_spent": summary.monthly_spent,
            "total_spent": summary.total_spent,
        },
        "agents": agent_list,
    })))
}

async fn agent_budget(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = BudgetManager::new(state.db.clone(), state.config());
    let summary = mgr.get_summary(Some(&agent_id)).map_err(internal_error)?;

    // Include transparent-downgrade state so the UI can show the
    // "running on local model (budget)" chip when the sidecar has
    // swapped this agent's model (ADR-023 §6, task 9).
    let state_record = mgr.get_state(&agent_id).map_err(internal_error)?;

    let mut body = match serde_json::to_value(&summary) {
        Ok(Value::Object(m)) => Value::Object(m),
        _ => json!({}),
    };
    if let Value::Object(ref mut map) = body {
        map.insert(
            "degraded_model".to_string(),
            json!(state_record.degraded_model),
        );
        map.insert("is_paused".to_string(), json!(state_record.is_paused));
    }
    Ok(Json(body))
}

async fn usage_history(
    State(state): State<AppState>,
    Query(params): Query<UsageParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let tracker = CostTracker::new(state.db.clone());
    let limit = params.limit.unwrap_or(100);
    let records = tracker
        .get_usage(params.agent_id.as_deref(), limit)
        .map_err(internal_error)?;
    Ok(Json(json!(records)))
}

async fn resume_agent(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = BudgetManager::new(state.db.clone(), state.config());
    mgr.resume(&agent_id).map_err(internal_error)?;
    let state_after = mgr.get_state(&agent_id).map_err(internal_error)?;
    Ok(Json(json!({
        "agent_id": agent_id,
        "is_paused": state_after.is_paused,
        "resumed": true,
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

    use xpressclaw_core::budget::manager::BudgetManager;
    use xpressclaw_core::budget::tracker::CostTracker;
    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;

    use super::*;

    fn test_app() -> (Arc<Database>, Router) {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db.clone(),
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );
        let router = Router::new().nest("/budget", routes()).with_state(state);
        (db, router)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_budget_summary_empty() {
        let (_db, app) = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/budget")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["global"]["total_spent"], 0.0);
        assert_eq!(body["global"]["daily_spent"], 0.0);
        assert!(body["agents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_budget_with_usage() {
        let (db, app) = test_app();

        // Record some usage
        let tracker = CostTracker::new(db.clone());
        tracker
            .record("atlas", "claude-sonnet-4.5", 1000, 500, "chat", None)
            .unwrap();

        let mgr = BudgetManager::new(db, Arc::new(Config::load_default().unwrap()));
        mgr.update_spending("atlas", 0.01).unwrap();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/budget/atlas")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["total_spent"].as_f64().unwrap() > 0.0);
    }

    #[tokio::test]
    async fn test_usage_history() {
        let (db, app) = test_app();

        let tracker = CostTracker::new(db);
        tracker
            .record("atlas", "gpt-4o", 500, 200, "query", None)
            .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/budget/usage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
    }
}
