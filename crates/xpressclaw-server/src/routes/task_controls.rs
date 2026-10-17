//! Observations and explicit task preferences are separate. GET never mutates;
//! POST saves only an explicit edit and may apply it to an existing idle session.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use xpressclaw_core::{
    sessions::{NewEvent, SessionManager},
    tasks::{
        board::TaskBoard,
        controls::{self, ControlScope},
    },
    workers::{acp::AcpSessionControls, native},
};

use crate::state::AppState;

type ApiError = (StatusCode, Json<Value>);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct ControlContext {
    agent_id: String,
    runner_kind: String,
    native_session_id: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
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
    revision: Option<String>,
    pending: HashMap<String, Value>,
    model_pending: bool,
    has_preferences: bool,
    notice: String,
}

#[derive(Deserialize)]
pub(super) struct Edit {
    context: ControlContext,
    revision: Option<String>,
    #[serde(default)]
    config_id: String,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    reset: bool,
}

fn observed(snapshot: &ControlSnapshot, id: &str) -> Option<Value> {
    snapshot
        .config_options
        .as_array()?
        .iter()
        .find(|option| option["id"] == id)
        .and_then(|option| option.get("currentValue"))
        .cloned()
        .or_else(|| {
            (id == "mode")
                .then(|| snapshot.modes.get("currentModeId").cloned())
                .flatten()
        })
}

fn decorate(
    state: &AppState,
    task_id: &str,
    mut snapshot: ControlSnapshot,
) -> Result<ControlSnapshot, ApiError> {
    if let Some(prefs) = controls::preferences(&state.db, task_id).map_err(internal)? {
        snapshot.revision = Some(prefs.revision.clone());
        if snapshot.context.as_ref().is_some_and(|ctx| {
            ctx.agent_id == prefs.scope.agent_id && ctx.runner_kind == prefs.scope.runner_kind
        }) {
            snapshot.has_preferences = !prefs.values.is_empty();
            let acknowledged = snapshot.availability == Availability::Live
                || snapshot
                    .event_id
                    .is_some_and(|id| id > prefs.after_event_id);
            snapshot.pending = prefs
                .values
                .into_iter()
                .filter(|(id, value)| {
                    !acknowledged || observed(&snapshot, id).as_ref() != Some(value)
                })
                .collect();
            let catalog: AcpSessionControls = serde_json::from_value(
                json!({"config_options":snapshot.config_options,"modes":snapshot.modes}),
            )
            .map_err(internal)?;
            snapshot.model_pending = catalog.model_id().is_some_and(|id| {
                snapshot
                    .pending
                    .get(&id)
                    .is_some_and(|value| observed(&snapshot, &id).as_ref() != Some(value))
            });
        }
    }
    snapshot.notice = if !snapshot.pending.is_empty() {
        "Saved for the next turn; not yet applied."
    } else {
        match snapshot.availability {
            Availability::Live => "Applied to the connected session.",
            Availability::Cached => "Last observed controls; changes will apply on the next turn.",
            Availability::Busy => "Agent is busy; changes will apply on the next turn.",
            Availability::Unavailable => "This context has not advertised usable controls yet.",
        }
    }
    .into();
    Ok(snapshot)
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
    let Json(snapshot) = raw_get(State(state.clone()), Path(task_id.clone())).await?;
    Ok(Json(decorate(&state, &task_id, snapshot)?))
}

async fn raw_get(
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
        revision: None,
        pending: HashMap::new(),
        model_pending: false,
        has_preferences: false,
        notice: String::new(),
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

pub(super) async fn set(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    Json(edit): Json<Edit>,
) -> Result<Json<ControlSnapshot>, ApiError> {
    let _edit_guard = state.task_controls_lock.lock().await;
    TaskBoard::new(state.db.clone())
        .ensure_locally_managed(&task_id)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":e.to_string()})),
            )
        })?;
    let conflict = || {
        (
            StatusCode::CONFLICT,
            Json(json!({"error":"Control context changed; refresh and try again"})),
        )
    };
    let Json(snapshot) = get(State(state.clone()), Path(task_id.clone())).await?;
    if snapshot.context.as_ref() != Some(&edit.context) || snapshot.revision != edit.revision {
        return Err(conflict());
    }
    let catalog: AcpSessionControls = serde_json::from_value(
        json!({"config_options":snapshot.config_options,"modes":snapshot.modes}),
    )
    .map_err(internal)?;
    let (model, value) = if edit.reset {
        (false, Value::Null)
    } else {
        catalog
            .validate_edit(&edit.config_id, &edit.value)
            .map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":e.to_string()})),
                )
            })?
    };
    let mut dependent = catalog.dependent_ids();
    if !edit.reset && snapshot.model_pending && dependent.contains(&edit.config_id) {
        return Err((
            StatusCode::CONFLICT,
            Json(
                json!({"error":"Wait for the selected model to advertise its reasoning controls"}),
            ),
        ));
    }
    let previous = controls::preferences(&state.db, &task_id).map_err(internal)?;
    let scope = ControlScope {
        agent_id: edit.context.agent_id.clone(),
        runner_kind: edit.context.runner_kind.clone(),
    };
    let mut values = HashMap::new();
    if let Some(previous) = previous.filter(|p| p.scope == scope) {
        values = previous.values;
        dependent.extend(previous.dependent_ids);
    }
    if model {
        values.retain(|id, _| !dependent.contains(id));
    }
    if edit.reset {
        values.clear();
        dependent.clear();
    } else {
        values.insert(edit.config_id.clone(), value.clone());
    }
    // Save first: a newly claimed turn must see the preference before a live
    // edit can be acknowledged. Already active turns keep their frozen input.
    controls::save_preferences(
        &state.db,
        &task_id,
        scope,
        edit.revision.as_deref(),
        values,
        dependent,
    )
    .map_err(|e| match e {
        xpressclaw_core::error::Error::ConfigValidation(_) => conflict(),
        _ => internal(e),
    })?;
    let Json(current) = raw_get(State(state.clone()), Path(task_id.clone())).await?;
    let mut failure = None;
    if !edit.reset
        && current.availability == Availability::Live
        && current.context.as_ref() == Some(&edit.context)
    {
        if let Some(native_id) = edit.context.native_session_id.as_deref() {
            if let Some(process) = state
                .project_processes
                .connected_process(&edit.context.agent_id, &edit.context.runner_kind)
                .await
            {
                match process
                    .configure(native_id, Some((edit.config_id, value)))
                    .await
                {
                    Ok(controls) => {
                        let sessions = SessionManager::new(state.db.clone());
                        if let Some(owner) = sessions
                            .task_control_attempt(
                                &task_id,
                                &edit.context.agent_id,
                                &edit.context.runner_kind,
                                false,
                            )
                            .map_err(internal)?
                            .filter(|owner| owner.native_session_id.as_deref() == Some(native_id))
                        {
                            sessions.append_event(&edit.context.agent_id,NewEvent {attempt_id:Some(&owner.id),task_id:Some(&task_id),source_type:"user",source_id:Some("local-user"),event_type:"session_config_options",summary:"Applied explicit picker edit",payload:json!({"config_options":controls.config_options,"modes":controls.modes,"native_session_id":native_id})}).map_err(internal)?;
                        }
                    }
                    Err(error) => failure = Some(format!("Saved, but not applied live: {error}")),
                }
            }
        }
    }
    let Json(mut snapshot) = get(State(state.clone()), Path(task_id)).await?;
    if edit.reset {
        snapshot.notice = "Task preferences cleared; agent defaults apply on the next turn.".into();
    }
    if let Some(failure) = failure {
        snapshot.notice = failure;
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
    async fn explicit_edits_persist_without_messages_and_only_matching_observations_acknowledge() {
        let db = Arc::new(Database::open_memory().unwrap());
        xpressclaw_core::agents::registry::AgentRegistry::new(db.clone())
            .ensure("agent", "pi")
            .unwrap();
        let config = Config {
            agents: vec![AgentConfig {
                name: "agent".into(),
                backend: "pi".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let state = AppState::new(Arc::new(config), db.clone(), None, "test.yaml".into(), true);
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Explicit preferences".into(),
                agent_id: Some("agent".into()),
                ..Default::default()
            })
            .unwrap();
        let sessions = SessionManager::new(db.clone());
        sessions.ensure("agent", None).unwrap();
        db.with_conn(|conn|conn.execute("INSERT INTO work_attempts(id,session_id,task_id,runner,status,native_session_id) VALUES('a','agent',?1,'pi','completed','native')",[&task.id])).unwrap();
        let advertise = |model: &str, reason: &str| {
            sessions.append_event("agent",NewEvent{attempt_id:Some("a"),task_id:Some(&task.id),source_type:"acp",source_id:Some("pi"),event_type:"session_config_options",summary:"Catalog",payload:json!({"config_options":[
            {"id":"model","name":"Model","category":"model","type":"select","currentValue":model,"options":[{"value":"sol","name":"Sol"},{"value":"qwen","name":"Qwen"}]},
            {"id":"thought_level","name":"Thinking","category":"thought_level","type":"select","currentValue":reason,"options":[{"value":"high","name":"High"},{"value":"max","name":"Max"},{"value":"off","name":"Off"}]}
        ],"modes":null})}).unwrap()
        };
        advertise("sol", "high");
        let Json(initial) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        let context = initial.context.unwrap();
        let edit = |id: &str, value: &str, revision: Option<String>| Edit {
            context: context.clone(),
            config_id: id.into(),
            value: json!(value),
            revision,
            reset: false,
        };
        let Json(reasoning) = set(
            State(state.clone()),
            Path(task.id.clone()),
            Json(edit("thought_level", "max", None)),
        )
        .await
        .unwrap();
        assert_eq!(
            reasoning.pending,
            HashMap::from([("thought_level".into(), json!("max"))])
        );
        assert_eq!(
            controls::preferences(&db, &task.id)
                .unwrap()
                .unwrap()
                .values,
            reasoning.pending
        );
        let old_revision = reasoning.revision.clone();
        let Json(model) = set(
            State(state.clone()),
            Path(task.id.clone()),
            Json(edit("model", "QWEN", reasoning.revision)),
        )
        .await
        .unwrap();
        assert_eq!(
            model.pending,
            HashMap::from([("model".into(), json!("qwen"))])
        );
        assert!(model.model_pending);
        assert_eq!(observed(&model, "model"), Some(json!("sol")));
        for (request, status) in [
            (edit("model", "sol", old_revision), StatusCode::CONFLICT),
            (
                edit("model", "unknown", model.revision.clone()),
                StatusCode::BAD_REQUEST,
            ),
            (
                edit("thought_level", "max", model.revision.clone()),
                StatusCode::CONFLICT,
            ),
            (
                Edit {
                    context: ControlContext {
                        runner_kind: "codex".into(),
                        ..context.clone()
                    },
                    ..edit("model", "sol", model.revision.clone())
                },
                StatusCode::CONFLICT,
            ),
        ] {
            assert_eq!(
                set(State(state.clone()), Path(task.id.clone()), Json(request))
                    .await
                    .unwrap_err()
                    .0,
                status
            );
        }
        assert_eq!(
            controls::preferences(&db, &task.id)
                .unwrap()
                .unwrap()
                .revision,
            model.revision.unwrap()
        );
        let Json(reopened) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        assert_eq!(reopened.pending["model"], "qwen");
        advertise("qwen", "off");
        let Json(confirmed) = get(State(state.clone()), Path(task.id.clone()))
            .await
            .unwrap();
        assert!(confirmed.pending.is_empty());
        assert!(!confirmed.model_pending);
        assert_eq!(observed(&confirmed, "thought_level"), Some(json!("off")));
        assert!(confirmed.has_preferences);
        let reset = Edit {
            reset: true,
            ..edit("", "", confirmed.revision)
        };
        let Json(cleared) = set(State(state), Path(task.id.clone()), Json(reset))
            .await
            .unwrap();
        assert!(!cleared.has_preferences);
        assert!(cleared.pending.is_empty());
        assert!(controls::preferences(&db, &task.id)
            .unwrap()
            .unwrap()
            .values
            .is_empty());
        db.with_conn(|conn| {
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM task_messages", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM task_queue", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok::<_, xpressclaw_core::error::Error>(())
        })
        .unwrap();
    }

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
