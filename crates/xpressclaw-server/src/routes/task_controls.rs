//! A read-only control snapshot. Displayed ACP state is not user intent.
//! Opening this endpoint never creates a session, turn, container, or preference.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};
use xpressclaw_core::{sessions::SessionManager, tasks::board::TaskBoard, workers::native};

use crate::state::AppState;

type ApiError = (StatusCode, Json<Value>);

#[derive(Debug, Serialize)]
struct ControlContext {
    agent_id: String,
    runner_kind: String,
    native_session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Availability {
    Live,
    Cached,
    Busy,
    Unavailable,
}

#[derive(Debug, Serialize)]
pub(super) struct ControlSnapshot {
    context: Option<ControlContext>,
    availability: Availability,
    config_options: Value,
    modes: Value,
    event_id: Option<i64>,
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": error.to_string()})),
    )
}

pub(super) async fn get(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
) -> Result<Json<ControlSnapshot>, ApiError> {
    let task = TaskBoard::new(state.db.clone())
        .get(&task_id)
        .map_err(|error| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":error.to_string()})),
            )
        })?;
    let mut snapshot = ControlSnapshot {
        context: None,
        availability: Availability::Unavailable,
        config_options: json!([]),
        modes: Value::Null,
        event_id: None,
    };
    let Some(agent_id) = task.agent_id.as_deref() else {
        return Ok(Json(snapshot));
    };
    let config = state.config();
    let Some(agent) = config.agents.iter().find(|agent| agent.name == agent_id) else {
        return Ok(Json(snapshot));
    };
    let runner = native::resolve_runner_kind(agent).map_err(internal)?;
    snapshot.context = Some(ControlContext {
        agent_id: agent_id.into(),
        runner_kind: runner.clone(),
        native_session_id: None,
    });
    let sessions = SessionManager::new(state.db.clone());
    let busy = sessions.has_active_attempts(agent_id).map_err(internal)?;
    if busy {
        snapshot.availability = Availability::Busy;
    }
    let Some(attempt) = sessions
        .task_control_attempt(&task_id, agent_id, &runner, busy)
        .map_err(internal)?
    else {
        return Ok(Json(snapshot));
    };
    let Some(native_id) = attempt.native_session_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(Json(snapshot));
    };
    snapshot.context.as_mut().unwrap().native_session_id = Some(native_id.clone());
    if !busy {
        if let Some(process) = state
            .project_processes
            .connected_process(agent_id, &runner)
            .await
        {
            if let Ok(controls) = process.configure(&native_id, None).await {
                snapshot.availability = Availability::Live;
                snapshot.config_options = json!(controls.config_options);
                snapshot.modes = json!(controls.modes);
                return Ok(Json(snapshot));
            }
        }
    }
    if let Some(event) = sessions
        .task_control_event(
            &task_id,
            agent_id,
            &runner,
            &native_id,
            "session_config_options",
        )
        .map_err(internal)?
    {
        if !busy {
            snapshot.availability = Availability::Cached;
        }
        snapshot.config_options = event
            .payload
            .get("config_options")
            .filter(|value| value.is_array())
            .cloned()
            .unwrap_or_else(|| json!([]));
        snapshot.modes = event.payload.get("modes").cloned().unwrap_or(Value::Null);
        snapshot.event_id = Some(event.id);
        if let Some(mode) = sessions
            .task_control_event(&task_id, agent_id, &runner, &native_id, "session_mode")
            .map_err(internal)?
            .filter(|mode| mode.id > event.id)
        {
            if let Some(id) = mode.payload["currentModeId"]
                .as_str()
                .or_else(|| mode.payload["modeId"].as_str())
                .or_else(|| mode.payload["mode_id"].as_str())
            {
                if let Some(options) = snapshot.config_options.as_array_mut() {
                    for option in options {
                        if option["category"] == "mode" || option["id"] == "mode" {
                            option["currentValue"] = json!(id);
                        }
                    }
                }
                if snapshot.modes.is_object() {
                    snapshot.modes["currentModeId"] = json!(id);
                }
                snapshot.event_id = Some(mode.id);
            }
        }
    }
    Ok(Json(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xpressclaw_core::{
        config::{AgentConfig, Config},
        db::Database,
        sessions::NewEvent,
        tasks::board::CreateTask,
    };

    #[tokio::test]
    async fn snapshot_is_scoped_durable_and_read_only() {
        let db = Arc::new(Database::open_memory().unwrap());
        let config = Config {
            agents: vec![AgentConfig {
                name: "agent".into(),
                backend: "pi".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        xpressclaw_core::agents::registry::AgentRegistry::new(db.clone())
            .ensure("agent", "pi")
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Controls".into(),
                agent_id: Some("agent".into()),
                ..Default::default()
            })
            .unwrap();
        let state = AppState::new(Arc::new(config), db.clone(), None, "test.yaml".into(), true);
        let sessions = SessionManager::new(db.clone());
        sessions.ensure("agent", None).unwrap();
        for (id, runner, status) in [
            ("reusable", "pi", "completed"),
            ("abandoned", "pi", "failed"),
            ("cancelled", "pi", "cancelled"),
            ("obsolete", "codex", "completed"),
        ] {
            db.with_conn(|conn|conn.execute("INSERT INTO work_attempts(id,session_id,task_id,runner,status,native_session_id) VALUES(?1,'agent',?2,?3,?4,?1)",[id,task.id.as_str(),runner,status])).unwrap();
            sessions.append_event("agent",NewEvent {attempt_id:Some(id),task_id:Some(&task.id),source_type:"acp",source_id:Some(runner),event_type:"session_config_options",summary:"Catalog",payload:json!({"config_options":[{"id":"model","currentValue":id},{"id":"mode","currentValue":"old"}],"modes":{"currentModeId":"old","availableModes":[]}})}).unwrap();
            sessions
                .append_event(
                    "agent",
                    NewEvent {
                        attempt_id: Some(id),
                        task_id: Some(&task.id),
                        source_type: "acp",
                        source_id: Some(runner),
                        event_type: "session_mode",
                        summary: "Mode",
                        payload: json!({"currentModeId":format!("{id}-mode")}),
                    },
                )
                .unwrap();
        }
        for _ in 0..125 {
            sessions
                .append_event(
                    "agent",
                    NewEvent {
                        attempt_id: None,
                        task_id: None,
                        source_type: "acp",
                        source_id: Some("pi"),
                        event_type: "progress",
                        summary: "Unrelated activity",
                        payload: json!({}),
                    },
                )
                .unwrap();
        }
        let before = sessions.list_events("agent", None, 1000).unwrap().len();
        let Json(snapshot) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(snapshot.config_options[0]["currentValue"], "reusable");
        assert_eq!(snapshot.modes["currentModeId"], "reusable-mode");
        assert!(matches!(snapshot.availability, Availability::Cached));
        assert_eq!(
            snapshot.context.unwrap().native_session_id.as_deref(),
            Some("reusable")
        );
        assert_eq!(
            sessions.list_events("agent", None, 1000).unwrap().len(),
            before
        );
        assert!(state
            .project_processes
            .connected_process("agent", "pi")
            .await
            .is_none());
        db.with_conn(|conn| {
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM task_queue",[],|row|row.get::<_,i64>(0))?,0);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM task_messages",[],|row|row.get::<_,i64>(0))?,0);
            conn.execute("UPDATE work_attempts SET status='running', native_session_id=NULL WHERE id='abandoned'",[])
        }).unwrap();
        let Json(opening) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        assert!(matches!(opening.availability, Availability::Busy));
        assert_eq!(opening.config_options, json!([]));
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET native_session_id='abandoned' WHERE id='abandoned'",
                [],
            )
        })
        .unwrap();
        let Json(busy) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        assert!(matches!(busy.availability, Availability::Busy));
        assert_eq!(busy.config_options[0]["currentValue"], "abandoned");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET status='failed' WHERE id='abandoned'",
                [],
            )
        })
        .unwrap();
        let Json(recovered) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(recovered.config_options[0]["currentValue"], "reusable");
        // A failed turn on the SAME reusable session may update its controls.
        // Exclude abandoned forks, not every failed event owner.
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET native_session_id='reusable' WHERE id='abandoned'",
                [],
            )
        })
        .unwrap();
        let Json(same_session) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(same_session.config_options[0]["currentValue"], "abandoned");
        assert_eq!(same_session.modes["currentModeId"], "abandoned-mode");
        db.with_conn(|conn| conn.execute("UPDATE tasks SET agent_id=NULL WHERE id=?1", [&task.id]))
            .unwrap();
        let Json(unassigned) = get(State(state), Path(task.id)).await.unwrap();
        assert!(unassigned.context.is_none());
        assert_eq!(unassigned.config_options, json!([]));
    }
}
