use std::collections::BTreeSet;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::conversations::event_bus::ConversationEvent;
use xpressclaw_core::projects::{
    CreateProject, ProjectDeletionPlan, ProjectManager, UpdateProject,
};
use xpressclaw_core::tasks::board::TaskBoard;
use xpressclaw_core::workers::acp::AcpInterruptMode;
use xpressclaw_core::workers::native;

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_projects).post(create_project))
        .route(
            "/{id}",
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
        )
        .route("/{id}/tasks", get(list_project_tasks))
        .route("/{id}/agents/{agent_id}", axum::routing::put(assign_agent))
}

async fn list_project_tasks(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    ProjectManager::new(state.db.clone())
        .get(&id)
        .map_err(project_error)?;
    let tasks = TaskBoard::new(state.db.clone())
        .list_for_project(&id, 100)
        .map_err(project_error)?;
    Ok(Json(json!(tasks)))
}

async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let projects = ProjectManager::new(state.db.clone())
        .list()
        .map_err(project_error)?;
    Ok(Json(json!(projects)))
}

async fn create_project(
    State(state): State<AppState>,
    Json(request): Json<CreateProject>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .create(&request)
        .map_err(project_error)?;
    Ok((StatusCode::CREATED, Json(json!(project))))
}

async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .get(&id)
        .map_err(project_error)?;
    Ok(Json(json!(project)))
}

async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateProject>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .update(&id, &request)
        .map_err(project_error)?;
    Ok(Json(json!(project)))
}

async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteProjectQuery>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    match query.cascade.as_deref() {
        None => {
            ProjectManager::new(state.db.clone())
                .delete(&id)
                .map_err(project_error)?;
            Ok(StatusCode::NO_CONTENT)
        }
        Some("confirmed") => delete_project_cascade(&state, &id).await,
        Some(_) => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "invalid cascade acknowledgement; use cascade=confirmed to permanently delete a populated Project"
            })),
        )),
    }
}

#[derive(Default, Deserialize)]
struct DeleteProjectQuery {
    cascade: Option<String>,
}

async fn delete_project_cascade(
    state: &AppState,
    id: &str,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    // Sync already uses sync-then-config lock ordering. Holding both through
    // the cascade prevents a fetch/publish or Agent config write from
    // reintroducing Project-owned state between the durable marker and final
    // removal.
    let _sync_guard = state.project_sync_lock.lock().await;
    let _config_guard = state.config_write_lock.lock().await;
    let manager = ProjectManager::new(state.db.clone());
    let plan = manager.begin_cascade(id).map_err(project_error)?;

    interrupt_project_work(state, &plan).await;
    remove_project_containers(state, &plan).await?;
    for runtime_id in plan
        .active_attempt_ids
        .iter()
        .chain(plan.active_turn_ids.iter())
    {
        state.turn_controls.finish_attempt(runtime_id);
    }

    let old_config = state.config();
    let mut new_config = (*old_config).clone();
    remove_project_agents_from_config(&mut new_config, &plan);
    new_config.save(&state.config_path).map_err(|error| {
        project_cleanup_error(format!(
            "Project work was stopped, but xpressclaw.yaml could not be updated: {error}. Fix the file permissions and retry deletion"
        ))
    })?;
    let new_config = std::sync::Arc::new(new_config);
    state.apply_config(new_config, state.llm_router());

    for agent in &plan.agents {
        native::remove_agent_runtime_state(&old_config.system.data_dir, &agent.id).map_err(
            |error| {
                project_cleanup_error(format!(
                    "Project configuration was updated, but Agent runtime files could not be removed: {error}. Retry deletion after fixing the path permissions"
                ))
            },
        )?;
    }

    manager.finish_cascade(id).map_err(project_error)?;
    for conversation_id in &plan.conversation_ids {
        state
            .event_bus
            .send(conversation_id, ConversationEvent::Done);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn remove_project_agents_from_config(
    config: &mut xpressclaw_core::config::Config,
    plan: &ProjectDeletionPlan,
) {
    let deleted_agent_ids = plan
        .agents
        .iter()
        .map(|agent| agent.id.as_str())
        .collect::<BTreeSet<_>>();
    config
        .agents
        .retain(|agent| !deleted_agent_ids.contains(agent.name.as_str()));
    config
        .collaboration
        .authorized_agents
        .retain(|agent| !deleted_agent_ids.contains(agent.as_str()));
}

async fn interrupt_project_work(state: &AppState, plan: &ProjectDeletionPlan) {
    for task_id in &plan.task_ids {
        state.elicitations.cancel_task(task_id);
    }
    for attempt_id in &plan.active_attempt_ids {
        state
            .turn_controls
            .request_interrupt(attempt_id, AcpInterruptMode::Immediate);
        state.elicitations.cancel_attempt(attempt_id);
    }
    for turn_id in &plan.active_turn_ids {
        state
            .turn_controls
            .request_interrupt(turn_id, AcpInterruptMode::Immediate);
        state.elicitations.cancel_attempt(turn_id);
    }
    for conversation_id in &plan.conversation_ids {
        state
            .conversation_processes
            .retire_conversation(conversation_id)
            .await;
    }
    for agent in &plan.agents {
        state
            .conversation_processes
            .retire_agent_everywhere(&agent.id)
            .await;
    }
}

async fn remove_project_containers(
    state: &AppState,
    plan: &ProjectDeletionPlan,
) -> Result<(), (StatusCode, Json<Value>)> {
    if plan.agents.is_empty()
        && plan.active_attempt_ids.is_empty()
        && plan.app_ids.is_empty()
        && plan.recorded_container_ids.is_empty()
    {
        return Ok(());
    }
    let docker = state.docker().await.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "Project work was cancelled, but Docker or Podman is unavailable. Start the container engine and retry deletion so XpressClaw can remove retained Agent runtimes safely. Source repositories and workspace folders have not been deleted."
            })),
        )
    })?;
    for container_id in &plan.recorded_container_ids {
        docker
            .remove_recorded_workload(container_id)
            .await
            .map_err(|error| {
                project_cleanup_error(format!(
                    "Project work was cancelled, but recorded runtime '{container_id}' could not be removed: {error}. Fix Docker or Podman and retry deletion"
                ))
            })?;
    }
    let mut workload_ids = BTreeSet::new();
    for agent in &plan.agents {
        workload_ids.insert(agent.id.clone());
    }
    workload_ids.extend(
        plan.active_attempt_ids
            .iter()
            .map(|attempt_id| format!("attempt-{attempt_id}")),
    );
    workload_ids.extend(plan.app_ids.iter().map(|app_id| format!("app-{app_id}")));
    for workload_id in workload_ids {
        docker
            .remove_owned_workload(&workload_id)
            .await
            .map_err(|error| {
            project_cleanup_error(format!(
                "Project work was cancelled, but runtime '{workload_id}' could not be removed: {error}. Fix Docker or Podman and retry deletion"
            ))
        })?;
    }
    Ok(())
}

fn project_cleanup_error(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message.into() })),
    )
}

#[derive(Deserialize)]
struct Assignment {
    #[serde(default)]
    _acknowledge: bool,
}

async fn assign_agent(
    State(state): State<AppState>,
    Path((id, agent_id)): Path<(String, String)>,
    _request: Option<Json<Assignment>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let project = ProjectManager::new(state.db.clone())
        .assign_agent(&id, &agent_id)
        .map_err(project_error)?;
    Ok(Json(json!(project)))
}

fn project_error(error: xpressclaw_core::error::Error) -> (StatusCode, Json<Value>) {
    let status = match error {
        xpressclaw_core::error::Error::ProjectNotFound { .. }
        | xpressclaw_core::error::Error::AgentNotFound { .. } => StatusCode::NOT_FOUND,
        xpressclaw_core::error::Error::Project(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({ "error": error.to_string() })))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tempfile::TempDir;
    use tower::ServiceExt;
    use xpressclaw_core::config::{AgentConfig, Config};
    use xpressclaw_core::db::Database;

    use super::*;

    fn app() -> Router {
        let state = AppState::new(
            Arc::new(Config::load_default().unwrap()),
            Arc::new(Database::open_memory().unwrap()),
            None,
            "test.yaml".into(),
            true,
        );
        routes().with_state(state)
    }

    fn isolated_app() -> (Router, Arc<Database>, AppState, TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.system.data_dir = root.path().join("data");
        let config_path = root.path().join("xpressclaw.yaml");
        config.save(&config_path).unwrap();
        let db = Arc::new(Database::open_memory().unwrap());
        let state = AppState::new(Arc::new(config), db.clone(), None, config_path, true);
        (routes().with_state(state.clone()), db, state, root)
    }

    #[tokio::test]
    async fn creates_and_lists_projects() {
        let app = app();
        let response = app
            .clone()
            .oneshot(
                Request::post("/")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name":"Website"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let response = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let projects: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(projects.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn populated_project_requires_exact_cascade_acknowledgement() {
        let (app, db, _, _root) = isolated_app();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO tasks (id, title, project_id)
                    VALUES ('task-one', 'Keep until confirmed', 'one');",
            )
        })
        .unwrap();

        let response = app
            .clone()
            .oneshot(Request::delete("/one").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let response = app
            .clone()
            .oneshot(
                Request::delete("/one?cascade=yes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let project = ProjectManager::new(db).get("one").unwrap();
        assert!(project.deletion_started_at.is_none());
        assert_eq!(project.task_count, 1);
    }

    #[tokio::test]
    async fn bare_delete_remains_compatible_for_empty_projects() {
        let (app, db, _, _root) = isolated_app();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO projects (id, name) VALUES ('empty', 'Empty')",
                [],
            )
        })
        .unwrap();

        let response = app
            .oneshot(Request::delete("/empty").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(matches!(
            ProjectManager::new(db).get("empty"),
            Err(xpressclaw_core::error::Error::ProjectNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn confirmed_cascade_removes_owned_rows_and_handles_last_project() {
        let (app, db, _, _root) = isolated_app();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One'), ('two', 'Two');
                 INSERT INTO conversations (id, title, project_id)
                    VALUES ('conversation-one', 'Delete conversation', 'one');
                 INSERT INTO conversation_messages
                    (conversation_id, sender_type, sender_id, content)
                    VALUES ('conversation-one', 'user', 'local', 'Delete message');
                 INSERT INTO tasks (id, title, conversation_id, project_id)
                    VALUES ('task-one', 'Delete task', 'conversation-one', 'one');
                 INSERT INTO task_messages (task_id, role, content)
                    VALUES ('task-one', 'user', 'Delete task message');
                 INSERT INTO project_memory_notes
                    (id, project_id, title, body, summary, search_key)
                    VALUES ('note-one', 'one', 'Delete note', 'body', 'summary', 'delete note');
                 INSERT INTO workflows (id, name, yaml_content)
                    VALUES ('shared', 'Shared workflow', 'name: Shared');
                 INSERT INTO workflow_instances
                    (id, workflow_id, status, project_id, conversation_id)
                    VALUES ('run-one', 'shared', 'completed', 'one', 'conversation-one'),
                           ('run-two', 'shared', 'completed', 'two', NULL);",
            )
        })
        .unwrap();

        let project_response = app
            .clone()
            .oneshot(Request::get("/one").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let project_body = project_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let project: Value = serde_json::from_slice(&project_body).unwrap();
        assert_eq!(project["deletion_counts"]["tasks"], 1);
        assert_eq!(project["deletion_counts"]["task_messages"], 1);
        assert_eq!(project["deletion_counts"]["conversations"], 1);
        assert_eq!(project["deletion_counts"]["conversation_messages"], 1);
        assert_eq!(project["deletion_counts"]["memory_notes"], 1);
        assert_eq!(project["deletion_counts"]["workflow_runs"], 1);

        let response = app
            .clone()
            .oneshot(
                Request::delete("/one?cascade=confirmed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let (projects, workflows, runs, tasks, conversations, notes): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = db.with_conn(|conn| {
            (
                conn.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
                    .unwrap(),
                conn.query_row("SELECT COUNT(*) FROM workflows", [], |row| row.get(0))
                    .unwrap(),
                conn.query_row("SELECT COUNT(*) FROM workflow_instances", [], |row| {
                    row.get(0)
                })
                .unwrap(),
                conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))
                    .unwrap(),
                conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
                    .unwrap(),
                conn.query_row("SELECT COUNT(*) FROM project_memory_notes", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            )
        });
        assert_eq!((projects, workflows, runs), (1, 1, 1));
        assert_eq!((tasks, conversations, notes), (0, 0, 0));

        let retry = app
            .clone()
            .oneshot(
                Request::delete("/one?cascade=confirmed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::NOT_FOUND);

        let last = app
            .clone()
            .oneshot(
                Request::delete("/two?cascade=confirmed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(last.status(), StatusCode::NO_CONTENT);
        let list = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = list.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!([]));
    }

    #[tokio::test]
    async fn failed_cleanup_keeps_the_marker_and_a_confirmed_retry_finishes() {
        let root = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.system.data_dir = root.path().join("data");
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('one', 'One');
                 INSERT INTO tasks (id, title, status, project_id)
                    VALUES ('task-one', 'Cancel me', 'in_progress', 'one');",
            )
        })
        .unwrap();

        let broken_state = AppState::new(
            Arc::new(config.clone()),
            db.clone(),
            None,
            root.path().to_path_buf(),
            true,
        );
        let response = routes()
            .with_state(broken_state)
            .oneshot(
                Request::delete("/one?cascade=confirmed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let project = ProjectManager::new(db.clone()).get("one").unwrap();
        assert!(project.deletion_started_at.is_some());
        let task_status: String = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT status FROM tasks WHERE id = 'task-one'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(task_status, "cancelled");

        let valid_config_path = root.path().join("xpressclaw.yaml");
        config.save(&valid_config_path).unwrap();
        let retry_state =
            AppState::new(Arc::new(config), db.clone(), None, valid_config_path, true);
        let response = routes()
            .with_state(retry_state)
            .oneshot(
                Request::delete("/one?cascade=confirmed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(matches!(
            ProjectManager::new(db).get("one"),
            Err(xpressclaw_core::error::Error::ProjectNotFound { .. })
        ));
    }

    #[test]
    fn config_cleanup_uses_stable_ids_when_a_display_name_collides() {
        let agents = ["agent-id", "atlas", "other"]
            .into_iter()
            .map(|name| AgentConfig {
                name: name.to_string(),
                ..AgentConfig::default()
            })
            .collect();
        let collaboration = xpressclaw_core::collaboration::CollaborationConfig {
            authorized_agents: vec!["atlas".into(), "agent-id".into(), "other".into()],
            ..Default::default()
        };
        let mut config = Config {
            agents,
            collaboration,
            ..Config::default()
        };
        let plan = ProjectDeletionPlan {
            project_id: "one".into(),
            project_name: "One".into(),
            agents: vec![xpressclaw_core::projects::ProjectDeletionAgent {
                id: "agent-id".into(),
                name: "atlas".into(),
                container_id: None,
            }],
            recorded_container_ids: vec![],
            conversation_ids: vec![],
            task_ids: vec![],
            active_attempt_ids: vec![],
            active_turn_ids: vec![],
            app_ids: vec![],
        };

        remove_project_agents_from_config(&mut config, &plan);

        assert_eq!(
            config
                .agents
                .iter()
                .map(|agent| agent.name.as_str())
                .collect::<Vec<_>>(),
            vec!["atlas", "other"]
        );
        assert_eq!(
            config.collaboration.authorized_agents,
            vec!["atlas", "other"]
        );
    }
}
