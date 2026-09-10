use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use xpressclaw_core::{
    agents::registry::AgentRegistry,
    docker::{
        environment::{
            check_forward_reservations, save_forwards, saved_forwards, ForwardDirection,
            PortForward,
        },
        manager::DockerManager,
    },
    tasks::{
        attachments::DecodedImageAttachment,
        board::{CreateTask, TaskBoard},
        conversation::TaskConversation,
        queue::TaskQueue,
    },
};

type ApiError = (StatusCode, Json<Value>);
type ApiResult<T> = Result<T, ApiError>;
fn bad(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error.to_string()})),
    )
}
fn conflict(error: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":error.to_string()})),
    )
}
fn forbidden() -> ApiError {
    (
        StatusCode::FORBIDDEN,
        Json(
            json!({"error":"This operation must target the calling Agent and its current project"}),
        ),
    )
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/{agent_id}/ports", get(list_ports).post(add_port))
        .route("/{agent_id}/ports/{id}", delete(remove_port))
        .route("/{agent_id}/tree", get(tree))
        .route("/{agent_id}/file", get(read_file).put(write_file))
        .route("/{agent_id}/download", get(download))
        .route("/{agent_id}/terminal-sessions", get(terminal_sessions))
}

pub fn internal_routes() -> Router<AppState> {
    Router::new()
        .route("/{agent_id}/tasks", post(create_agent_task))
        .route(
            "/{agent_id}/tasks/{task_id}/files",
            post(publish_task_files),
        )
}

fn authorize(state: &AppState, agent_id: &str, headers: &HeaderMap) -> ApiResult<()> {
    super::workspace::require_same_origin(headers)?;
    if headers
        .get("x-xpressclaw-agent-id")
        .is_some_and(|value| value.to_str().ok() != Some(agent_id))
    {
        return Err(forbidden());
    }
    if !state
        .config()
        .agents
        .iter()
        .any(|agent| agent.name == agent_id)
    {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Agent not found"})),
        ));
    }
    Ok(())
}

fn require_agent(headers: &HeaderMap, agent_id: &str) -> ApiResult<()> {
    if headers
        .get("x-xpressclaw-agent-id")
        .and_then(|value| value.to_str().ok())
        != Some(agent_id)
    {
        return Err(forbidden());
    }
    Ok(())
}

async fn docker(state: &AppState) -> ApiResult<std::sync::Arc<DockerManager>> {
    state.docker().await.ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"Docker or Podman is not available"})),
        )
    })
}

async fn active_docker(
    state: &AppState,
    agent_id: &str,
) -> ApiResult<std::sync::Arc<DockerManager>> {
    let docker = docker(state).await?;
    docker
        .start_project_environment(agent_id)
        .await
        .map_err(bad)?;
    docker.restore_forwards(&state.db, agent_id).await;
    Ok(docker)
}

#[derive(Deserialize)]
struct PortInput {
    direction: ForwardDirection,
    host_port: u16,
    container_port: u16,
}

async fn list_ports(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    let docker = state.docker().await;
    let mut ports = vec![];
    for spec in saved_forwards(&state.db, &agent_id).map_err(bad)? {
        let active = match &docker {
            Some(docker) => docker.forward_active(&agent_id, &spec.id).await,
            None => false,
        };
        let mut value = json!(spec);
        value["active"] = json!(active);
        ports.push(value);
    }
    Ok(Json(json!({"ports":ports})))
}

async fn add_port(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<PortInput>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    let spec = PortForward {
        id: uuid::Uuid::new_v4().to_string(),
        direction: input.direction,
        host_port: input.host_port,
        container_port: input.container_port,
    };
    spec.validate().map_err(bad)?;
    check_forward_reservations(&state.db, &agent_id, std::slice::from_ref(&spec))
        .map_err(conflict)?;
    let docker = docker(&state).await?;
    let _forwards_lock = docker.lock_agent_forwards(&agent_id).await;
    let mut ports = saved_forwards(&state.db, &agent_id).map_err(bad)?;
    if let Some(existing) = ports.iter().find(|port| {
        port.direction == spec.direction
            && port.host_port == spec.host_port
            && port.container_port == spec.container_port
    }) {
        save_forwards(&state.db, &agent_id, &ports).map_err(conflict)?;
        if docker.is_running(&agent_id).await {
            docker
                .start_forward(&agent_id, existing)
                .await
                .map_err(conflict)?;
        }
        return Ok(forward_result(
            existing,
            docker.forward_active(&agent_id, &existing.id).await,
        ));
    }
    if ports.len() >= 16 {
        return Err(bad("An environment can have at most 16 port forwards"));
    }
    if ports.iter().any(|port| {
        port.direction == spec.direction
            && match spec.direction {
                ForwardDirection::HostToContainer => port.container_port == spec.container_port,
                ForwardDirection::ContainerToHost => port.host_port == spec.host_port,
            }
    }) {
        return Err(bad("This listening port already has a mapping"));
    }
    let active = docker.is_running(&agent_id).await;
    if !active && spec.direction == ForwardDirection::ContainerToHost {
        // Detect occupied host listeners even when the container is stopped.
        tokio::net::TcpListener::bind(("127.0.0.1", spec.host_port))
            .await
            .map_err(|error| {
                conflict(format!("Cannot bind host port {}: {error}", spec.host_port))
            })?;
    }
    let previous = ports.clone();
    ports.push(spec.clone());
    // Reserve host ports atomically before doing Docker I/O. Other Agents can
    // save independent mappings without waiting for this bridge's readiness.
    save_forwards(&state.db, &agent_id, &ports).map_err(conflict)?;
    if active {
        if let Err(error) = docker.start_forward(&agent_id, &spec).await {
            save_forwards(&state.db, &agent_id, &previous).map_err(bad)?;
            return Err(conflict(error));
        }
    }
    Ok(forward_result(&spec, active))
}

fn forward_result(spec: &PortForward, active: bool) -> Json<Value> {
    Json(
        json!({"id":spec.id, "direction":spec.direction, "host_port":spec.host_port, "container_port":spec.container_port, "active":active,
        "host_address":format!("127.0.0.1:{}", spec.host_port), "container_address":format!("127.0.0.1:{}", spec.container_port),
        "message":if active { "Port forward is active. Host addresses refer to the machine running XpressClaw." } else { "Saved. The forward starts when this Agent's container starts." }}),
    )
}

async fn remove_port(
    State(state): State<AppState>,
    Path((agent_id, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    let docker = state.docker().await;
    let _forwards_lock = match docker.as_ref() {
        Some(docker) => Some(docker.lock_agent_forwards(&agent_id).await),
        None => None,
    };
    let mut ports = saved_forwards(&state.db, &agent_id).map_err(bad)?;
    ports.retain(|port| port.id != id);
    if let Some(docker) = docker.as_ref() {
        docker.stop_forward(&agent_id, &id).await;
    }
    save_forwards(&state.db, &agent_id, &ports).map_err(bad)?;
    Ok(Json(json!({"removed":true})))
}

#[derive(Deserialize)]
struct FileQuery {
    #[serde(default = "root_path")]
    path: String,
}
fn root_path() -> String {
    "/".into()
}

async fn tree(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    Ok(Json(
        active_docker(&state, &agent_id)
            .await?
            .container_files(&agent_id, json!({"operation":"tree", "path":query.path}))
            .await
            .map_err(bad)?,
    ))
}
async fn read_file(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    Ok(Json(
        active_docker(&state, &agent_id)
            .await?
            .container_files(&agent_id, json!({"operation":"read", "path":query.path}))
            .await
            .map_err(bad)?,
    ))
}
#[derive(Deserialize)]
struct SaveFile {
    path: String,
    content: String,
    expected_revision: String,
}
async fn write_file(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<SaveFile>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    Ok(Json(docker(&state).await?.container_files(&agent_id, json!({"operation":"write", "path":input.path, "content":input.content, "expected_revision":input.expected_revision})).await.map_err(bad)?))
}

async fn download(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<FileQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    authorize(&state, &agent_id, &headers)?;
    let value = active_docker(&state, &agent_id)
        .await?
        .container_files(
            &agent_id,
            json!({"operation":"download", "path":query.path}),
        )
        .await
        .map_err(bad)?;
    let data = STANDARD
        .decode(value["data"].as_str().unwrap_or(""))
        .map_err(bad)?;
    let filename: String = value["name"]
        .as_str()
        .unwrap_or("download")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ".-_ ".contains(ch) {
                ch
            } else {
                '_'
            }
        })
        .collect();
    Response::builder()
        .header(
            header::CONTENT_TYPE,
            value["mime_type"]
                .as_str()
                .unwrap_or("application/octet-stream"),
        )
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(Body::from(data))
        .map_err(bad)
}

async fn terminal_sessions(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    Ok(Json(
        active_docker(&state, &agent_id)
            .await?
            .container_files(&agent_id, json!({"operation":"sessions"}))
            .await
            .map_err(bad)?,
    ))
}

#[derive(Deserialize)]
struct AgentTaskInput {
    title: String,
    description: Option<String>,
    agent_id: Option<String>,
    parent_task_id: Option<String>,
    priority: Option<i32>,
}
async fn create_agent_task(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<AgentTaskInput>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    require_agent(&headers, &agent_id)?;
    let registry = AgentRegistry::new(state.db.clone());
    let caller = registry.get(&agent_id).map_err(bad)?;
    let target_id = input.agent_id.as_deref().unwrap_or(&agent_id);
    let target = registry.get(target_id).map_err(bad)?;
    if caller.project_id.is_none() || caller.project_id != target.project_id {
        return Err(forbidden());
    }
    let board = TaskBoard::new(state.db.clone());
    if let Some(parent_id) = &input.parent_task_id {
        let parent = board.get(parent_id).map_err(bad)?;
        if parent.project_id != caller.project_id {
            return Err(forbidden());
        }
    }
    let task = board
        .create(&CreateTask {
            title: input.title,
            description: input.description,
            agent_id: Some(target_id.into()),
            parent_task_id: input.parent_task_id,
            priority: input.priority,
            ..Default::default()
        })
        .map_err(bad)?;
    TaskQueue::new(state.db.clone())
        .enqueue(&task.id, target_id)
        .map_err(bad)?;
    Ok(Json(json!(task)))
}

#[derive(Deserialize)]
struct PublishInput {
    #[serde(default)]
    content: String,
    files: Vec<String>,
}
async fn publish_task_files(
    State(state): State<AppState>,
    Path((agent_id, task_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<PublishInput>,
) -> ApiResult<Json<Value>> {
    authorize(&state, &agent_id, &headers)?;
    require_agent(&headers, &agent_id)?;
    let task = TaskBoard::new(state.db.clone())
        .get(&task_id)
        .map_err(bad)?;
    if task.agent_id.as_deref() != Some(&agent_id) {
        return Err(forbidden());
    }
    if input.files.is_empty() || input.files.len() > 8 {
        return Err(bad("Publish between 1 and 8 files or folders"));
    }
    let docker = docker(&state).await?;
    let files = collect_published_files(&input.files, |request| {
        docker.container_files(&agent_id, request)
    })
    .await?;
    let message = TaskConversation::new(state.db.clone())
        .add_message_with_attachments(&task_id, "assistant", &input.content, &files)
        .map_err(bad)?;
    Ok(Json(json!(message)))
}

async fn collect_published_files<F, Fut>(
    paths: &[String],
    mut read: F,
) -> ApiResult<Vec<DecodedImageAttachment>>
where
    F: FnMut(Value) -> Fut,
    Fut: std::future::Future<Output = xpressclaw_core::error::Result<Value>>,
{
    const LIMIT: usize = 20 * 1024 * 1024;
    const TOO_LARGE: &str =
        "Published files cannot exceed 20 MiB per message; download larger folders from Files";
    let mut remaining = LIMIT;
    // Preflight the whole request before materializing even its first file.
    for path in paths {
        let info = read(json!({"operation":"stat", "path":path, "max_bytes":remaining}))
            .await
            .map_err(bad)?;
        let size = info["size"]
            .as_u64()
            .ok_or_else(|| bad("Container did not return the file size"))?;
        if size > remaining as u64 {
            return Err(bad(TOO_LARGE));
        }
        remaining -= size as usize;
    }
    let mut files = vec![];
    remaining = LIMIT;
    for path in paths {
        // Recheck and bound reads/tar in the container as files can grow after
        // preflight. The host JSON response limit uses this same byte budget.
        let value = read(json!({"operation":"download", "path":path, "max_bytes":remaining}))
            .await
            .map_err(bad)?;
        let encoded = value["data"]
            .as_str()
            .ok_or_else(|| bad("Container did not return file data"))?;
        if encoded.len() > remaining.div_ceil(3) * 4 {
            return Err(bad(TOO_LARGE));
        }
        let data = STANDARD.decode(encoded).map_err(bad)?;
        if data.len() > remaining {
            return Err(bad(TOO_LARGE));
        }
        remaining -= data.len();
        files.push(DecodedImageAttachment {
            name: value["name"].as_str().unwrap_or("file").into(),
            mime_type: value["mime_type"]
                .as_str()
                .unwrap_or("application/octet-stream")
                .into(),
            data,
        });
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;
    use xpressclaw_core::{
        config::{AgentConfig, Config},
        db::Database,
    };

    fn app() -> (Router, Arc<Database>) {
        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::default();
        for name in ["atlas", "helper", "outsider"] {
            AgentRegistry::new(db.clone())
                .ensure(name, "native")
                .unwrap();
            config.agents.push(AgentConfig {
                name: name.into(),
                ..Default::default()
            });
        }
        db.with_conn(|conn| {
            conn.execute_batch("INSERT INTO projects (id, name) VALUES ('shared', 'Shared'), ('other', 'Other'); UPDATE agents SET project_id = 'shared' WHERE id IN ('atlas', 'helper'); UPDATE agents SET project_id = 'other' WHERE id = 'outsider';")
        }).unwrap();
        let state = AppState::new(
            Arc::new(config),
            db.clone(),
            None,
            "/tmp/environment-test.yaml".into(),
            true,
        );
        (routes().merge(internal_routes()).with_state(state), db)
    }

    #[tokio::test]
    async fn cross_agent_host_port_conflicts_return_an_actionable_http_error() {
        let (app, db) = app();
        save_forwards(
            &db,
            "helper",
            &[PortForward {
                id: "server".into(),
                direction: ForwardDirection::ContainerToHost,
                host_port: 3000,
                container_port: 3001,
            }],
        )
        .unwrap();
        let response = app.oneshot(Request::post("/atlas/ports")
            .header("content-type", "application/json")
            .body(Body::from(json!({"direction":"host_to_container", "host_port":3000, "container_port":8080}).to_string())).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(value["error"]
            .as_str()
            .unwrap()
            .contains("Host port 3000 is reserved by Agent helper"));
        assert!(saved_forwards(&db, "atlas").unwrap().is_empty());
    }

    #[tokio::test]
    async fn publishing_checks_all_sizes_before_any_download() {
        let mut operations = vec![];
        let result = collect_published_files(&["/tmp/small".into(), "/tmp/large".into()], |request| {
            operations.push(request.clone());
            std::future::ready(Ok(json!({"size":if request["path"] == "/tmp/large" { 21 * 1024 * 1024 } else { 10 }})))
        }).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(operations.len(), 2);
        assert!(operations
            .iter()
            .all(|request| request["operation"] == "stat"));
        assert_eq!(operations[1]["max_bytes"], 20 * 1024 * 1024 - 10);
    }

    #[tokio::test]
    async fn publishing_bounds_downloads_by_the_remaining_message_budget() {
        let mut operations = vec![];
        let files = collect_published_files(&["/tmp/a".into(), "/tmp/b".into()], |request| {
            operations.push(request.clone());
            std::future::ready(Ok(if request["operation"] == "stat" {
                json!({"size":2})
            } else {
                json!({"name":"file", "data":"YWJj", "mime_type":"text/plain"})
            }))
        })
        .await
        .unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].data, b"abc");
        assert_eq!(operations[2]["operation"], "download");
        assert_eq!(operations[2]["max_bytes"], 20 * 1024 * 1024);
        assert_eq!(operations[3]["max_bytes"], 20 * 1024 * 1024 - 3);
    }

    #[tokio::test]
    async fn creates_and_queues_a_blocking_child_in_the_callers_project() {
        let (app, db) = app();
        let parent = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Parent".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let response = app
            .oneshot(
                Request::post("/atlas/tasks")
                    .header("x-xpressclaw-agent-id", "atlas")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"title":"Child", "agent_id":"helper", "parent_task_id":parent.id})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["project_id"], "shared");
        assert_eq!(value["parent_task_id"], parent.id);
        assert_eq!(value["blocks_parent"], true);
        assert_eq!(value["agent_id"], "helper");
        let queued: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM task_queue WHERE task_id = ?1",
                    [value["id"].as_str().unwrap()],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(queued, 1);
    }

    #[tokio::test]
    async fn rejects_cross_agent_environment_access_and_cross_project_delegation() {
        let (app, db) = app();
        for url in [
            "/helper/ports",
            "/helper/tree?path=/tmp",
            "/helper/download?path=/tmp",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::get(url)
                        .header("x-xpressclaw-agent-id", "atlas")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let parent = TaskBoard::new(db)
            .create(&CreateTask {
                title: "Other parent".into(),
                agent_id: Some("outsider".into()),
                ..Default::default()
            })
            .unwrap();
        for input in [
            json!({"title":"Wrong assignee", "agent_id":"outsider"}),
            json!({"title":"Wrong parent", "parent_task_id":parent.id}),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::post("/atlas/tasks")
                        .header("x-xpressclaw-agent-id", "atlas")
                        .header("content-type", "application/json")
                        .body(Body::from(input.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
}
