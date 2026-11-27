use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/", get(get_usage))
}

#[derive(Debug, Default, Deserialize)]
struct UsageQuery {
    #[serde(default)]
    refresh: bool,
    #[serde(default)]
    provider: Option<String>,
}

async fn get_usage(State(state): State<AppState>, Query(query): Query<UsageQuery>) -> Json<Value> {
    let snapshot = state
        .usage_service
        .snapshot(&state.db, query.refresh, query.provider.as_deref())
        .await;
    Json(json!(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;
    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;
    use xpressclaw_core::usage::{window_from_used_pct, UsageAccountReport, UsageReportStatus};

    #[tokio::test]
    async fn usage_endpoint_returns_accounts_and_deferred_automations() {
        let db = Arc::new(Database::open_memory().unwrap());
        let state = AppState::new(
            Arc::new(Config::default()),
            db.clone(),
            None,
            std::path::PathBuf::from("xpressclaw.yaml"),
            true,
        );
        state
            .usage_service
            .set_provider_override(
                "claude",
                vec![UsageAccountReport {
                    id: "claude:test-acct".into(),
                    provider: "claude".into(),
                    provider_label: "Claude".into(),
                    harness: "Claude".into(),
                    account_label: Some("dev@example.com".into()),
                    status: UsageReportStatus::Available,
                    plan_label: Some("Max 5x".into()),
                    observed_at: None,
                    fetched_at: "2026-11-26T12:00:00Z".into(),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "Session",
                        Some("5h".into()),
                        true,
                        Some(42.0),
                        Some(18_000),
                        Some("2026-11-26T16:00:00Z".into()),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO workflows (id, name, yaml_content, enabled, version, quota_deferred_until, quota_deferred_reason)
                 VALUES ('wf-1', 'Nightly audit', 'name: Nightly\nflows:\n  main:\n    steps:\n      - id: s1\n        agent: a\n        prompt: hi', 1, 1, '2099-01-01T00:00:00Z', 'claude Session has 4% remaining (needs ≥20%)')",
                [],
            )
        })
        .unwrap();

        let app = routes().with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/?provider=claude")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(body["accounts"][0]["provider"], "claude");
        assert_eq!(body["accounts"][0]["windows"][0]["used_pct"], 42.0);
        assert_eq!(body["deferred_automations"].as_array().unwrap().len(), 1);
        assert_eq!(body["deferred_automations"][0]["name"], "Nightly audit");
        assert_eq!(body["deferred_automations"][0]["provider"], "claude");
    }
}
