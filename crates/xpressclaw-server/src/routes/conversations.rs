use std::convert::Infallible;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

use xpressclaw_core::agents::registry::AgentRegistry;
use xpressclaw_core::budget::manager::BudgetManager;
use xpressclaw_core::budget::rate_limiter::RateLimitResult;
use xpressclaw_core::budget::tracker::CostTracker;
use xpressclaw_core::conversations::{ConversationManager, CreateConversation, SendMessage};
use xpressclaw_core::llm::router::{ChatCompletionRequest, ChatMessage};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct MessageParams {
    pub limit: Option<i64>,
    pub before_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct SendMessageInput {
    pub content: String,
    pub sender_name: Option<String>,
}

#[derive(Deserialize)]
pub struct AddParticipantInput {
    pub participant_type: String,
    pub participant_id: String,
}

#[derive(Deserialize)]
pub struct UpdateConversationInput {
    pub title: Option<String>,
    pub icon: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_conversations).post(create_conversation))
        .route(
            "/{id}",
            get(get_conversation)
                .patch(update_conversation)
                .delete(delete_conversation),
        )
        .route("/{id}/messages", get(get_messages).post(send_message))
        .route("/{id}/messages/stream", axum::routing::post(stream_message))
        .route(
            "/{id}/participants",
            get(get_participants).post(add_participant),
        )
        .route(
            "/{id}/participants/{participant_id}",
            axum::routing::delete(remove_participant),
        )
}

async fn list_conversations(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    let convs = mgr
        .list(params.limit.unwrap_or(50))
        .map_err(internal_error)?;
    Ok(Json(json!(convs)))
}

async fn create_conversation(
    State(state): State<AppState>,
    Json(req): Json<CreateConversation>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    let conv = mgr.create(&req).map_err(internal_error)?;
    Ok((StatusCode::CREATED, Json(json!(conv))))
}

async fn get_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    let conv = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ConversationNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        ),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(conv)))
}

async fn update_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateConversationInput>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    let conv = mgr
        .update(&id, req.title.as_deref(), req.icon.as_deref())
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::ConversationNotFound { .. } => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": e.to_string() })),
            ),
            _ => internal_error(e),
        })?;
    Ok(Json(json!(conv)))
}

async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    mgr.delete(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ConversationNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        ),
        _ => internal_error(e),
    })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<MessageParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    let msgs = mgr
        .get_messages(&id, params.limit.unwrap_or(50), params.before_id)
        .map_err(internal_error)?;
    Ok(Json(json!(msgs)))
}

async fn send_message(
    State(state): State<AppState>,
    Path(conv_id): Path<String>,
    Json(req): Json<SendMessageInput>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());

    // Store user message
    let user_msg = mgr
        .send_message(
            &conv_id,
            &SendMessage {
                sender_type: "user".into(),
                sender_id: "local".into(),
                sender_name: req.sender_name.clone(),
                content: req.content.clone(),
                message_type: None,
            },
        )
        .map_err(internal_error)?;

    let mut messages = vec![json!(user_msg)];

    // Resolve which agents should respond
    let target_agents = mgr
        .resolve_target_agents(&conv_id, &req.content)
        .map_err(internal_error)?;

    if let Some(llm_router) = state.llm_router() {
        let registry = AgentRegistry::new(state.db.clone());
        let budget_mgr = BudgetManager::new(state.db.clone(), state.config());
        let cost_tracker = CostTracker::new(state.db.clone());
        let rate_limiter = state.rate_limiter();

        for agent_id in &target_agents {
            // Check budget before calling LLM
            match budget_mgr.check_budget(agent_id) {
                Ok(false) => {
                    let err_msg = mgr
                        .send_message(
                            &conv_id,
                            &SendMessage {
                                sender_type: "agent".into(),
                                sender_id: agent_id.clone(),
                                sender_name: Some(agent_id.clone()),
                                content: format!(
                                    "*Budget exceeded for agent {}. Message not sent.*",
                                    agent_id
                                ),
                                message_type: Some("system".into()),
                            },
                        )
                        .map_err(internal_error)?;
                    messages.push(json!(err_msg));
                    continue;
                }
                Err(e) => {
                    let err_msg = mgr
                        .send_message(
                            &conv_id,
                            &SendMessage {
                                sender_type: "agent".into(),
                                sender_id: agent_id.clone(),
                                sender_name: Some(agent_id.clone()),
                                content: format!("*{}*", e),
                                message_type: Some("system".into()),
                            },
                        )
                        .map_err(internal_error)?;
                    messages.push(json!(err_msg));
                    continue;
                }
                Ok(true) => {} // within budget
            }

            // Check rate limits
            if let RateLimitResult::RequestsExceeded { limit, .. }
            | RateLimitResult::TokensExceeded { limit, .. } = rate_limiter.check(agent_id)
            {
                let err_msg = mgr
                    .send_message(
                        &conv_id,
                        &SendMessage {
                            sender_type: "agent".into(),
                            sender_id: agent_id.clone(),
                            sender_name: Some(agent_id.clone()),
                            content: format!(
                                "*Rate limit reached for agent {} ({}/min). Please wait.*",
                                agent_id, limit
                            ),
                            message_type: Some("system".into()),
                        },
                    )
                    .map_err(internal_error)?;
                messages.push(json!(err_msg));
                continue;
            }

            // Look up agent to get model config
            let agent = match registry.get(agent_id) {
                Ok(a) => a,
                Err(_) => continue,
            };

            let model = agent.config["model"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| {
                    llm_router
                        .models()
                        .first()
                        .map(|m| m.id.clone())
                        .unwrap_or_else(|| "local".to_string())
                });

            let role = agent.config["role"]
                .as_str()
                .unwrap_or("You are a helpful AI assistant.");

            let history = mgr.get_messages(&conv_id, 20, None).unwrap_or_default();

            let mut llm_messages = vec![ChatMessage {
                role: "system".into(),
                content: role.to_string(),
            }];

            for m in &history {
                let r = match m.sender_type.as_str() {
                    "agent" => "assistant",
                    _ => "user",
                };
                llm_messages.push(ChatMessage {
                    role: r.to_string(),
                    content: m.content.clone(),
                });
            }

            let llm_req = ChatCompletionRequest {
                model: model.clone(),
                messages: llm_messages,
                temperature: Some(0.7),
                max_tokens: Some(4096),
                stream: Some(false),
                top_p: None,
                stop: None,
            };

            match llm_router.chat(&llm_req).await {
                Ok(resp) => {
                    // Record usage and update budget
                    if let Some(ref usage) = resp.usage {
                        let _ = cost_tracker.record(
                            agent_id,
                            &model,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            "chat",
                            Some(&conv_id),
                        );
                        let cost = cost_tracker.pricing().calculate(
                            &model,
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            0,
                            0,
                        );
                        let _ = budget_mgr.update_spending(agent_id, cost);
                        rate_limiter.record_request(agent_id, usage.total_tokens as u32);
                    }

                    if let Some(choice) = resp.choices.first() {
                        let agent_msg = mgr
                            .send_message(
                                &conv_id,
                                &SendMessage {
                                    sender_type: "agent".into(),
                                    sender_id: agent_id.clone(),
                                    sender_name: Some(agent_id.clone()),
                                    content: choice.message.content.clone(),
                                    message_type: None,
                                },
                            )
                            .map_err(internal_error)?;
                        messages.push(json!(agent_msg));
                    }
                }
                Err(e) => {
                    let err_msg = mgr
                        .send_message(
                            &conv_id,
                            &SendMessage {
                                sender_type: "agent".into(),
                                sender_id: agent_id.clone(),
                                sender_name: Some(agent_id.clone()),
                                content: format!("*Error: {e}*"),
                                message_type: Some("system".into()),
                            },
                        )
                        .map_err(internal_error)?;
                    messages.push(json!(err_msg));
                }
            }
        }
    }

    Ok((StatusCode::CREATED, Json(json!(messages))))
}

/// Streaming version of send_message. Returns SSE events:
/// - `user_message`: the stored user message
/// - `thinking`: agent is about to generate a response
/// - `chunk`: a token chunk from the agent
/// - `agent_message`: the final stored agent message
/// - `done`: all agents have responded
/// - `error`: an error occurred
async fn stream_message(
    State(state): State<AppState>,
    Path(conv_id): Path<String>,
    Json(req): Json<SendMessageInput>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());

    // Store user message
    let user_msg = mgr
        .send_message(
            &conv_id,
            &SendMessage {
                sender_type: "user".into(),
                sender_id: "local".into(),
                sender_name: req.sender_name.clone(),
                content: req.content.clone(),
                message_type: None,
            },
        )
        .map_err(internal_error)?;

    // Resolve which agents should respond
    let target_agents = mgr
        .resolve_target_agents(&conv_id, &req.content)
        .map_err(internal_error)?;

    let llm_router = state.llm_router();
    let db = state.db.clone();
    let config = state.config();
    let rate_limiter = state.rate_limiter();

    let stream = async_stream::stream! {
        // Send user message event
        if let Ok(evt) = Event::default().event("user_message").json_data(json!(user_msg)) {
            yield Ok(evt);
        }

        let Some(llm_router) = llm_router else {
            if let Ok(evt) = Event::default().event("error").json_data(json!({"error": "LLM router not configured"})) {
                yield Ok(evt);
            }
            if let Ok(evt) = Event::default().event("done").json_data(json!({})) {
                yield Ok(evt);
            }
            return;
        };

        let registry = AgentRegistry::new(db.clone());
        let mgr = ConversationManager::new(db.clone());
        let budget_mgr = BudgetManager::new(db.clone(), config);
        let cost_tracker = CostTracker::new(db.clone());

        for agent_id in &target_agents {
            // Check budget
            match budget_mgr.check_budget(agent_id) {
                Ok(false) => {
                    if let Ok(evt) = Event::default().event("error").json_data(json!({
                        "agent_id": agent_id,
                        "error": format!("Budget exceeded for agent {}", agent_id)
                    })) {
                        yield Ok(evt);
                    }
                    continue;
                }
                Err(e) => {
                    if let Ok(evt) = Event::default().event("error").json_data(json!({
                        "agent_id": agent_id,
                        "error": e.to_string()
                    })) {
                        yield Ok(evt);
                    }
                    continue;
                }
                Ok(true) => {}
            }

            // Check rate limits
            match rate_limiter.check(agent_id) {
                RateLimitResult::RequestsExceeded { limit, .. } => {
                    if let Ok(evt) = Event::default().event("error").json_data(json!({
                        "agent_id": agent_id,
                        "error": format!("Rate limit reached ({} requests/min)", limit)
                    })) {
                        yield Ok(evt);
                    }
                    continue;
                }
                RateLimitResult::TokensExceeded { limit, .. } => {
                    if let Ok(evt) = Event::default().event("error").json_data(json!({
                        "agent_id": agent_id,
                        "error": format!("Token rate limit reached ({} tokens/min)", limit)
                    })) {
                        yield Ok(evt);
                    }
                    continue;
                }
                RateLimitResult::Allowed => {}
            }

            let agent = match registry.get(agent_id) {
                Ok(a) => a,
                Err(_) => continue,
            };

            let model = agent.config["model"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| {
                    llm_router
                        .models()
                        .first()
                        .map(|m| m.id.clone())
                        .unwrap_or_else(|| "local".to_string())
                });

            let role = agent.config["role"]
                .as_str()
                .unwrap_or("You are a helpful AI assistant.");

            let history = mgr.get_messages(&conv_id, 20, None).unwrap_or_default();

            let mut llm_messages = vec![ChatMessage {
                role: "system".into(),
                content: role.to_string(),
            }];

            for m in &history {
                let r = match m.sender_type.as_str() {
                    "agent" => "assistant",
                    _ => "user",
                };
                llm_messages.push(ChatMessage {
                    role: r.to_string(),
                    content: m.content.clone(),
                });
            }

            let llm_req = ChatCompletionRequest {
                model: model.clone(),
                messages: llm_messages,
                temperature: Some(0.7),
                max_tokens: Some(4096),
                stream: Some(true),
                top_p: None,
                stop: None,
            };

            // Send "thinking" event
            if let Ok(evt) = Event::default().event("thinking").json_data(json!({
                "agent_id": agent_id
            })) {
                yield Ok(evt);
            }

            match llm_router.chat_stream(&llm_req).await {
                Ok(mut chunk_stream) => {
                    let mut full_content = String::new();
                    let mut total_tokens: i64 = 0;

                    while let Some(result) = chunk_stream.next().await {
                        match result {
                            Ok(chunk) => {
                                if let Some(choice) = chunk.choices.first() {
                                    if let Some(ref text) = choice.delta.content {
                                        full_content.push_str(text);
                                        total_tokens += 1; // approximate: 1 chunk ≈ 1 token
                                        if let Ok(evt) = Event::default().event("chunk").json_data(json!({
                                            "agent_id": agent_id,
                                            "content": text
                                        })) {
                                            yield Ok(evt);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                if let Ok(evt) = Event::default().event("error").json_data(json!({
                                    "agent_id": agent_id,
                                    "error": e.to_string()
                                })) {
                                    yield Ok(evt);
                                }
                                break;
                            }
                        }
                    }

                    // Record usage
                    if !full_content.is_empty() {
                        // Estimate input tokens from message count (rough approximation)
                        let input_tokens = (llm_req.messages.iter().map(|m| m.content.len()).sum::<usize>() / 4) as i64;
                        let output_tokens = total_tokens;
                        let _ = cost_tracker.record(
                            agent_id, &model, input_tokens, output_tokens, "chat", Some(&conv_id),
                        );
                        let cost = cost_tracker.pricing().calculate(&model, input_tokens, output_tokens, 0, 0);
                        let _ = budget_mgr.update_spending(agent_id, cost);
                        rate_limiter.record_request(agent_id, (input_tokens + output_tokens) as u32);

                        if let Ok(agent_msg) = mgr.send_message(
                            &conv_id,
                            &SendMessage {
                                sender_type: "agent".into(),
                                sender_id: agent_id.clone(),
                                sender_name: Some(agent_id.clone()),
                                content: full_content,
                                message_type: None,
                            },
                        ) {
                            if let Ok(evt) = Event::default().event("agent_message").json_data(json!(agent_msg)) {
                                yield Ok(evt);
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = mgr.send_message(
                        &conv_id,
                        &SendMessage {
                            sender_type: "agent".into(),
                            sender_id: agent_id.clone(),
                            sender_name: Some(agent_id.clone()),
                            content: format!("*Error: {e}*"),
                            message_type: Some("system".into()),
                        },
                    );
                    if let Ok(evt) = Event::default().event("error").json_data(json!({
                        "agent_id": agent_id,
                        "error": e.to_string()
                    })) {
                        yield Ok(evt);
                    }
                }
            }
        }

        // Done
        if let Ok(evt) = Event::default().event("done").json_data(json!({})) {
            yield Ok(evt);
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("ping"),
    ))
}

async fn get_participants(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    let conv = mgr.get(&id).map_err(|e| match &e {
        xpressclaw_core::error::Error::ConversationNotFound { .. } => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": e.to_string() })),
        ),
        _ => internal_error(e),
    })?;
    Ok(Json(json!(conv.participants)))
}

async fn add_participant(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<AddParticipantInput>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    mgr.add_participant(&id, &req.participant_type, &req.participant_id)
        .map_err(|e| match &e {
            xpressclaw_core::error::Error::ConversationNotFound { .. } => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": e.to_string() })),
            ),
            _ => internal_error(e),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_participant(
    State(state): State<AppState>,
    Path((id, participant_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());
    mgr.remove_participant(&id, "agent", &participant_id)
        .map_err(internal_error)?;
    Ok(StatusCode::NO_CONTENT)
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

    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;

    use super::*;

    fn test_app() -> Router {
        let db = Arc::new(Database::open_memory().unwrap());
        // Register a test agent
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agents (id, name, backend, config) VALUES ('atlas', 'atlas', 'generic', '{\"role\": \"You are atlas.\"}')",
                [],
            )
            .unwrap();
        });
        let config = Arc::new(Config::load_default().unwrap());
        let state = AppState::new(
            config,
            db,
            None,
            std::path::PathBuf::from("test.yaml"),
            true,
        );

        Router::new()
            .nest("/conversations", routes())
            .with_state(state)
    }

    async fn body_json(body: Body) -> Value {
        let bytes = body.collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_create_and_list() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/conversations")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "title": "General",
                            "participant_ids": ["atlas"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["title"], "General");
        assert_eq!(body["participants"].as_array().unwrap().len(), 2);

        // List
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/conversations")
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
    async fn test_send_and_get_messages() {
        let app = test_app();

        // Create conversation
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/conversations")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"title": "Test", "participant_ids": ["atlas"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let conv_id = body["id"].as_str().unwrap().to_string();

        // Send message (no LLM router → only user message returned)
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/conversations/{conv_id}/messages"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"content": "Hello atlas!", "sender_name": "Eduardo"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp.into_body()).await;
        let msgs = body.as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "Hello atlas!");
        assert_eq!(msgs[0]["sender_name"], "Eduardo");

        // Get messages
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/conversations/{conv_id}/messages"))
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
    async fn test_delete_conversation() {
        let app = test_app();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/conversations")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"title": "Delete me"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let id = body["id"].as_str().unwrap().to_string();

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/conversations/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/conversations/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_not_found() {
        let app = test_app();

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/conversations/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
