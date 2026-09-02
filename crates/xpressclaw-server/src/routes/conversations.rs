use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::conversations::event_bus::ConversationEvent;
use xpressclaw_core::conversations::runtime::ConversationTurnQueue;
use xpressclaw_core::conversations::{
    ConversationManager, CreateConversation, NewConversationAttachment, SendMessage,
};
use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
use xpressclaw_core::visualizations::VisualizationManager;
use xpressclaw_core::workers::acp::AcpInterruptMode;
use xpressclaw_core::workflows::engine::{WorkflowContext, WorkflowEngine};

use crate::state::AppState;

use super::visualizations;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_conversations).post(create_conversation))
        .route(
            "/{id}",
            get(get_conversation)
                .patch(update_conversation)
                .delete(delete_conversation),
        )
        .route("/{id}/participants", post(add_participant))
        .route(
            "/{id}/participants/{agent_id}",
            axum::routing::delete(remove_participant),
        )
        .route("/{id}/messages", get(list_messages).post(send_user_message))
        .route("/{id}/message-deletions", get(list_message_deletions))
        .route("/{id}/messages/{message_id}", delete(delete_message))
        .route("/{id}/agent-messages", post(send_agent_message))
        .route("/{id}/events", get(conversation_events))
        .route(
            "/{id}/tasks",
            get(list_linked_tasks).post(create_linked_task),
        )
        .route("/{id}/turns", get(list_turns))
        .route("/{id}/turns/{turn_id}/cancel", post(cancel_turn))
        .route("/{id}/attachments/{attachment_id}", get(get_attachment))
        .route(
            "/{id}/messages/{message_id}/visualizations/{artifact_id}",
            get(get_visualization),
        )
}

#[derive(Deserialize)]
struct ListConversations {
    project_id: Option<String>,
    limit: Option<i64>,
}

async fn list_conversations(
    State(state): State<AppState>,
    Query(query): Query<ListConversations>,
) -> ApiResult {
    let conversations = ConversationManager::new(state.db.clone())
        .list_in_project(
            query.project_id.as_deref(),
            query.limit.unwrap_or(100).clamp(1, 200),
        )
        .map_err(api_error)?;
    Ok(Json(json!(conversations)))
}

#[derive(Deserialize)]
struct CreateConversationRequest {
    project_id: String,
    title: Option<String>,
    icon: Option<String>,
    #[serde(default)]
    participant_ids: Vec<String>,
}

async fn create_conversation(
    State(state): State<AppState>,
    Json(request): Json<CreateConversationRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    validate_project_agents(&state, &request.project_id, &request.participant_ids)?;
    let conversation = ConversationManager::new(state.db.clone())
        .create_in_project(
            Some(&request.project_id),
            &CreateConversation {
                title: request.title,
                icon: request.icon,
                participant_ids: request.participant_ids,
            },
        )
        .map_err(api_error)?;
    Ok((StatusCode::CREATED, Json(json!(conversation))))
}

async fn get_conversation(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let conversation = ConversationManager::new(state.db.clone())
        .get(&id)
        .map_err(api_error)?;
    Ok(Json(json!(conversation)))
}

#[derive(Deserialize)]
struct UpdateConversationRequest {
    title: Option<String>,
    icon: Option<String>,
}

async fn update_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateConversationRequest>,
) -> ApiResult {
    let conversation = ConversationManager::new(state.db.clone())
        .update(&id, request.title.as_deref(), request.icon.as_deref())
        .map_err(api_error)?;
    Ok(Json(json!(conversation)))
}

async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    ConversationManager::new(state.db.clone())
        .delete_with_running_turns(&id, |turn_id| {
            state
                .turn_controls
                .request_interrupt(turn_id, AcpInterruptMode::Immediate);
            state.elicitations.cancel_attempt(turn_id);
        })
        .map_err(api_error)?;
    state.conversation_processes.retire_conversation(&id).await;
    state.event_bus.send(&id, ConversationEvent::Done);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ParticipantRequest {
    agent_id: String,
}

async fn add_participant(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ParticipantRequest>,
) -> ApiResult {
    let manager = ConversationManager::new(state.db.clone());
    manager
        .add_participant(&id, "agent", &request.agent_id)
        .map_err(api_error)?;
    Ok(Json(json!(manager.get(&id).map_err(api_error)?)))
}

async fn remove_participant(
    State(state): State<AppState>,
    Path((id, agent_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let running_turns = ConversationManager::new(state.db.clone())
        .remove_participant(&id, "agent", &agent_id)
        .map_err(api_error)?;
    for turn_id in running_turns {
        state
            .turn_controls
            .request_interrupt(&turn_id, AcpInterruptMode::Immediate);
        state.elicitations.cancel_attempt(&turn_id);
    }
    state
        .conversation_processes
        .retire_agent(&id, &agent_id)
        .await;
    state.event_bus.send(&id, ConversationEvent::Done);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct MessageQuery {
    limit: Option<i64>,
    before_id: Option<i64>,
}

async fn list_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<MessageQuery>,
) -> ApiResult {
    let manager = ConversationManager::new(state.db.clone());
    let messages = manager
        .get_messages(
            &id,
            query.limit.unwrap_or(100).clamp(1, 200),
            query.before_id,
        )
        .map_err(api_error)?;
    let messages = messages
        .into_iter()
        .map(|message| {
            let attachments = manager.attachments(message.id).unwrap_or_default();
            let visualizations = manager.visualizations(message.id).unwrap_or_default();
            let mut value = json!(message);
            value["attachments"] = json!(attachments);
            value["visualizations"] = json!(visualizations);
            value
        })
        .collect::<Vec<_>>();
    Ok(Json(json!(messages)))
}

async fn list_message_deletions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult {
    let message_ids = ConversationManager::new(state.db.clone())
        .deleted_message_ids(&id)
        .map_err(api_error)?;
    Ok(Json(json!(message_ids)))
}

async fn delete_message(
    State(state): State<AppState>,
    Path((id, message_id)): Path<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    let running_turns = ConversationManager::new(state.db.clone())
        .delete_message(&id, message_id)
        .map_err(api_error)?;
    for turn_id in running_turns {
        state
            .turn_controls
            .request_interrupt(&turn_id, AcpInterruptMode::Immediate);
        state.elicitations.cancel_attempt(&turn_id);
    }
    state
        .event_bus
        .send(&id, ConversationEvent::MessageDeleted { message_id });
    state.event_bus.send(&id, ConversationEvent::Done);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AttachmentInput {
    name: String,
    mime_type: String,
    data: String,
}

fn decode_attachments(
    inputs: Vec<AttachmentInput>,
    source_task_id: Option<&str>,
) -> Result<Vec<NewConversationAttachment>, ApiError> {
    if inputs.len() > 10 {
        return Err(bad_request("a message can contain at most 10 attachments"));
    }
    let encoded_size = inputs
        .iter()
        .try_fold(0usize, |total, attachment| {
            total.checked_add(attachment.data.len())
        })
        .ok_or_else(|| bad_request("conversation attachments are too large"))?;
    if encoded_size > 28 * 1024 * 1024 {
        return Err(bad_request(
            "conversation attachments in one message must total 20 MiB or less",
        ));
    }
    inputs
        .into_iter()
        .map(|attachment| {
            let data = STANDARD
                .decode(attachment.data.trim())
                .map_err(|_| bad_request("attachment data is not valid base64"))?;
            Ok(NewConversationAttachment {
                name: attachment.name,
                mime_type: attachment.mime_type,
                data,
                source_task_id: source_task_id.map(str::to_owned),
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct SendUserMessageRequest {
    content: String,
    #[serde(default)]
    attachments: Vec<AttachmentInput>,
}

async fn send_user_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SendUserMessageRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if request.content.trim().is_empty() && request.attachments.is_empty() {
        return Err(bad_request("a message or attachment is required"));
    }
    let attachments = decode_attachments(request.attachments, None)?;
    let manager = ConversationManager::new(state.db.clone());
    let (message, attachments, queued_agents) = manager
        .send_routed_message_with_attachments(
            &id,
            &SendMessage {
                sender_type: "user".into(),
                sender_id: "local".into(),
                sender_name: Some("You".into()),
                content: request.content,
                message_type: None,
            },
            None,
            None,
            &attachments,
        )
        .map_err(api_error)?;
    let mut value = json!(message);
    value["attachments"] = json!(attachments);
    state.event_bus.send(
        &id,
        ConversationEvent::Message {
            message: value.clone(),
        },
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "message": value, "queued_agents": queued_agents })),
    ))
}

#[derive(Deserialize)]
struct SendAgentMessageRequest {
    agent_id: String,
    content: String,
    #[serde(default)]
    attachments: Vec<AttachmentInput>,
    source_task_id: Option<String>,
}

async fn send_agent_message(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<SendAgentMessageRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if request.content.trim().is_empty() && request.attachments.is_empty() {
        return Err(bad_request("a message or attachment is required"));
    }
    let attachments = decode_attachments(request.attachments, request.source_task_id.as_deref())?;
    let manager = ConversationManager::new(state.db.clone());
    let sender_name = state
        .config()
        .agents
        .iter()
        .find(|agent| agent.name == request.agent_id)
        .map(|agent| agent.context_label())
        .unwrap_or_else(|| request.agent_id.clone());
    let (message, attachments, queued_agents) = manager
        .send_agent_routed_message_with_attachments(
            &id,
            &SendMessage {
                sender_type: "agent".into(),
                sender_id: request.agent_id.clone(),
                sender_name: Some(sender_name),
                content: request.content,
                message_type: None,
            },
            request.source_task_id.as_deref(),
            &attachments,
        )
        .map_err(api_error)?;
    let mut value = json!(message);
    value["attachments"] = json!(attachments);
    state.event_bus.send(
        &id,
        ConversationEvent::Message {
            message: value.clone(),
        },
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "message": value, "queued_agents": queued_agents })),
    ))
}

#[derive(Deserialize)]
struct CreateLinkedTaskRequest {
    title: String,
    description: Option<String>,
    agent_id: Option<String>,
    creator_agent_id: Option<String>,
    workflow_id: Option<String>,
    #[serde(default = "empty_object")]
    workflow_inputs: Value,
    priority: Option<i32>,
}

async fn create_linked_task(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<CreateLinkedTaskRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let manager = ConversationManager::new(state.db.clone());
    let conversation = manager.get(&id).map_err(api_error)?;
    let project_id = conversation
        .project_id
        .as_deref()
        .ok_or_else(|| bad_request("conversation is not assigned to a project"))?;
    if let Some(creator_agent_id) = request.creator_agent_id.as_deref() {
        if request.agent_id.as_deref() != Some(creator_agent_id) {
            return Err(bad_request(
                "an Agent may only create a conversation task for itself",
            ));
        }
    }
    if let Some(agent_id) = request.agent_id.as_deref() {
        validate_project_agents(&state, project_id, &[agent_id.to_string()])?;
    }

    let (sender_type, sender_id, sender_name) =
        if let Some(agent_id) = request.creator_agent_id.as_deref() {
            let label = state
                .config()
                .agents
                .iter()
                .find(|agent| agent.name == agent_id)
                .map(|agent| agent.context_label())
                .unwrap_or_else(|| agent_id.to_string());
            ("agent", agent_id.to_string(), label)
        } else {
            ("user", "local".to_string(), "You".to_string())
        };

    if let Some(workflow_id) = request.workflow_id.as_deref() {
        let engine = WorkflowEngine::new(state.db.clone());
        let context = WorkflowContext {
            project_id: Some(project_id.to_string()),
            conversation_id: Some(id.clone()),
        };
        let (instance_id, message) = engine
            .start_instance_in_context_with_conversation_message(
                workflow_id,
                request.workflow_inputs,
                context,
                request.creator_agent_id.as_deref(),
                &SendMessage {
                    sender_type: sender_type.into(),
                    sender_id,
                    sender_name: Some(sender_name),
                    content: format!("Started workflow: {}", request.title),
                    message_type: Some("workflow".into()),
                },
            )
            .map_err(api_error)?;
        state.event_bus.send(
            &id,
            ConversationEvent::Message {
                message: json!(message),
            },
        );
        return Ok((
            StatusCode::CREATED,
            Json(json!({ "workflow_instance_id": instance_id })),
        ));
    }

    let create_task = CreateTask {
        title: request.title,
        description: request.description,
        agent_id: request.agent_id,
        parent_task_id: None,
        sop_id: None,
        conversation_id: Some(id.clone()),
        priority: request.priority,
        context: Some(json!({ "origin": "conversation", "project_id": project_id })),
    };
    let linked_message = SendMessage {
        sender_type: sender_type.into(),
        sender_id,
        sender_name: Some(sender_name),
        content: format!("Created task: {}", create_task.title),
        message_type: Some("task".into()),
    };
    let (task, message) = manager
        .create_linked_task_and_message(
            &id,
            &create_task,
            request.creator_agent_id.as_deref(),
            &linked_message,
        )
        .map_err(api_error)?;
    state.event_bus.send(
        &id,
        ConversationEvent::Message {
            message: json!(message),
        },
    );
    Ok((StatusCode::CREATED, Json(json!(task))))
}

async fn list_linked_tasks(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let tasks = TaskBoard::new(state.db.clone())
        .list_for_conversation(&id)
        .map_err(api_error)?;
    Ok(Json(json!(tasks)))
}

async fn list_turns(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let turns = ConversationTurnQueue::new(state.db.clone())
        .list_for_conversation(&id, 100)
        .map_err(api_error)?;
    Ok(Json(json!(turns)))
}

async fn cancel_turn(
    State(state): State<AppState>,
    Path((id, turn_id)): Path<(String, String)>,
) -> ApiResult {
    let cancellation = ConversationTurnQueue::new(state.db.clone())
        .cancel(&id, &turn_id)
        .map_err(api_error)?;
    if cancellation.was_running {
        state
            .turn_controls
            .request_interrupt(&turn_id, AcpInterruptMode::Immediate);
        state.elicitations.cancel_attempt(&turn_id);
    }
    if cancellation.changed {
        state.event_bus.send(&id, ConversationEvent::Done);
    }
    Ok(Json(json!(cancellation.turn)))
}

async fn get_attachment(
    State(state): State<AppState>,
    Path((id, attachment_id)): Path<(String, String)>,
) -> Result<Response<Body>, ApiError> {
    let (attachment, data) = ConversationManager::new(state.db.clone())
        .attachment_data(&id, &attachment_id)
        .map_err(api_error)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, attachment.mime_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                safe_filename(&attachment.name)
            ),
        )
        .header("x-content-type-options", "nosniff")
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .body(Body::from(data))
        .map_err(|error| api_error(error.to_string()))
}

async fn get_visualization(
    State(state): State<AppState>,
    Path((id, message_id, artifact_id)): Path<(String, i64, String)>,
    headers: HeaderMap,
) -> Result<Response<Body>, ApiError> {
    let token = visualizations::retrieval_token(&headers).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "visualization not found" })),
        )
    })?;
    let artifact = VisualizationManager::new(state.db.clone())
        .conversation_artifact(&id, message_id, &artifact_id, token)
        .map_err(api_error)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "visualization not found" })),
            )
        })?;
    visualizations::artifact_response(artifact).map_err(api_error)
}

async fn conversation_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.event_bus.subscribe(&id);
    let stream = BroadcastStream::new(receiver).filter_map(|result| match result {
        Ok(event) => Event::default().json_data(event).ok().map(Ok),
        Err(_) => None,
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

fn validate_project_agents(
    state: &AppState,
    project_id: &str,
    agent_ids: &[String],
) -> Result<(), ApiError> {
    let registry = AgentRegistry::new(state.db.clone());
    for agent_id in agent_ids {
        let belongs = registry
            .get(agent_id)
            .map(|agent| agent.project_id.as_deref() == Some(project_id))
            .unwrap_or(false);
        if !belongs {
            return Err(bad_request(format!(
                "Agent '{agent_id}' does not belong to this project"
            )));
        }
    }
    Ok(())
}

fn safe_filename(name: &str) -> String {
    name.chars()
        .map(|character| match character {
            '\r' | '\n' | '"' | '\\' => '_',
            other if other == ' ' || other.is_ascii_graphic() => other,
            _ => '_',
        })
        .collect()
}

fn empty_object() -> Value {
    json!({})
}

type ApiResult = Result<Json<Value>, ApiError>;
type ApiError = (StatusCode, Json<Value>);

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
}

fn api_error(error: impl std::fmt::Display) -> ApiError {
    let message = error.to_string();
    let status = if message.contains("not found") {
        StatusCode::NOT_FOUND
    } else if message.starts_with("conversation error:")
        || message.starts_with("project error:")
        || message.starts_with("workflow error:")
    {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, Json(json!({ "error": message })))
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

    fn app_with_db() -> (Router, Arc<Database>) {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO projects (id, name) VALUES ('project', 'Project')",
                [],
            )?;
            connection
                .execute(
                    "INSERT INTO agents (id, name, backend, config, project_id)
                     VALUES ('atlas', 'Atlas', 'native', '{}', 'project')",
                    [],
                )
                .map(|_| ())
        })
        .unwrap();
        let state = AppState::new(
            Arc::new(Config::load_default().unwrap()),
            db.clone(),
            None,
            "test.yaml".into(),
            true,
        );
        (routes().with_state(state), db)
    }

    fn app() -> Router {
        app_with_db().0
    }

    async fn json_body(response: Response<Body>) -> Value {
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn conversation_messages_files_and_tasks_share_one_project_context() {
        let app = app();
        let response = app
            .clone()
            .oneshot(
                Request::post("/")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"project_id":"project","title":"Launch","participant_ids":["atlas"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let conversation = json_body(response).await;
        let id = conversation["id"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/{id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"content":"Do not persist this","attachments":[{"name":"bad.txt","mime_type":"text/plain","data":"not base64"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/{id}/messages"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_body(response).await, json!([]));

        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/{id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"content":"Please investigate","attachments":[{"name":"brief.txt","mime_type":"text/plain","data":"aGVsbG8="}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let sent = json_body(response).await;
        assert_eq!(sent["queued_agents"], json!(["atlas"]));
        let attachment_id = sent["message"]["attachments"][0]["id"].as_str().unwrap();

        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/{id}/tasks"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"title":"Research","description":"Find the answer","agent_id":"atlas"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let task = json_body(response).await;
        assert_eq!(task["conversation_id"], id);

        let response = app
            .oneshot(
                Request::get(format!("/{id}/attachments/{attachment_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-disposition"],
            "attachment; filename=\"brief.txt\""
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "hello"
        );
    }

    #[tokio::test]
    async fn conversation_responses_can_be_cancelled_and_messages_deleted() {
        let (app, db) = app_with_db();
        let response = app
            .clone()
            .oneshot(
                Request::post("/")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"project_id":"project","title":"Recoverable","participant_ids":["atlas"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let conversation = json_body(response).await;
        let id = conversation["id"].as_str().unwrap();
        let sent = app
            .clone()
            .oneshot(
                Request::post(format!("/{id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"content":"Please answer"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let sent = json_body(sent).await;
        let message_id = sent["message"]["id"].as_i64().unwrap();
        let turns = app
            .clone()
            .oneshot(
                Request::get(format!("/{id}/turns"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let turns = json_body(turns).await;
        let turn_id = turns[0]["id"].as_str().unwrap();

        let cancelled = app
            .clone()
            .oneshot(
                Request::post(format!("/{id}/turns/{turn_id}/cancel"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status(), StatusCode::OK);
        assert_eq!(json_body(cancelled).await["status"], "cancelled");

        let deleted = app
            .clone()
            .oneshot(
                Request::delete(format!("/{id}/messages/{message_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
        let listed = app
            .clone()
            .oneshot(
                Request::get(format!("/{id}/messages"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_body(listed).await, json!([]));
        let deletions = app
            .oneshot(
                Request::get(format!("/{id}/message-deletions"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json_body(deletions).await, json!([message_id]));
        let tombstoned = db
            .with_conn(|connection| {
                connection.query_row(
                    "SELECT deleted_at IS NOT NULL FROM conversation_messages WHERE id = ?1",
                    [message_id],
                    |row| row.get::<_, bool>(0),
                )
            })
            .unwrap();
        assert!(tombstoned);
    }

    #[tokio::test]
    async fn conversation_visualizations_are_listed_and_retrieved_only_in_scope() {
        let (app, db) = app_with_db();
        let message_id = db
            .with_conn(
                |connection| -> std::result::Result<i64, xpressclaw_core::error::Error> {
                    connection.execute(
                        "INSERT INTO conversations (id, project_id, title)
                     VALUES ('visual-conversation', 'project', 'Visual'),
                            ('other-conversation', 'project', 'Other')",
                        [],
                    )?;
                    connection.execute(
                        "INSERT INTO conversation_messages
                     (conversation_id, sender_type, sender_id, content)
                     VALUES ('visual-conversation', 'agent', 'atlas', 'visual')",
                        [],
                    )?;
                    let message_id = connection.last_insert_rowid();
                    connection.execute(
                        "INSERT INTO message_visualizations
                     (id, conversation_message_id, reference_index, title,
                      display_mode, status, content, content_sha256, size, retrieval_token)
                     VALUES ('conversation-viz', ?1, 0, 'Conversation visual',
                             'wide', 'ready', '<div>conversation</div>',
                             '0000000000000000000000000000000000000000000000000000000000000000',
                             23, 'conversation-token')",
                        [message_id],
                    )?;
                    Ok(message_id)
                },
            )
            .unwrap();

        let listed = app
            .clone()
            .oneshot(
                Request::get("/visual-conversation/messages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed = json_body(listed).await;
        assert_eq!(listed[0]["visualizations"][0]["id"], "conversation-viz");
        assert_eq!(listed[0]["visualizations"][0]["mode"], "wide");

        let path =
            format!("/visual-conversation/messages/{message_id}/visualizations/conversation-viz");
        let response = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(visualizations::RETRIEVAL_TOKEN_HEADER, "conversation-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::get(format!(
                    "/other-conversation/messages/{message_id}/visualizations/conversation-viz"
                ))
                .header(visualizations::RETRIEVAL_TOKEN_HEADER, "conversation-token")
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
