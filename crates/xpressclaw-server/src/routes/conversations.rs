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
use xpressclaw_core::tasks::board::{CreateTask, TaskBoard};
use xpressclaw_core::tasks::queue::TaskQueue;

use crate::state::AppState;

/// Task continuation signal embedded in agent responses.
#[derive(Debug, serde::Deserialize)]
struct TaskSignal {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<i32>,
}

/// Extract a `__TASK__` continuation signal from agent response content.
/// Returns (clean_content, optional_task_signal).
fn extract_task_signal(content: &str) -> (String, Option<TaskSignal>) {
    if let Some(idx) = content.find("__TASK__") {
        let clean = content[..idx].trim().to_string();
        let json_str = content[idx + 8..].trim();
        let signal = serde_json::from_str(json_str).ok();
        (clean, signal)
    } else {
        (content.to_string(), None)
    }
}

/// Detect XML-style tool call attempts in model output.
///
/// Some models (e.g. Qwen) produce `<parameter=name>value</parameter>` or
/// `<tool_call>` with XML parameters instead of JSON tool calls. This happens
/// more as conversations get longer and the model forgets the correct format.
/// Returns a hint message if detected, so it can be injected into the
/// conversation and the model can retry.
fn detect_malformed_tool_call(content: &str) -> Option<String> {
    // Match <parameter=...>...</parameter> patterns
    if content.contains("<parameter=") && content.contains("</parameter>") {
        return Some(
            "[System: Your last message used XML-style `<parameter=name>value</parameter>` \
             syntax for a tool call. This format is not supported. Please retry using the \
             correct JSON format inside `<tool_call name=\"tool_name\">{...}</tool_call>` tags. \
             For example:\n\
             <tool_call name=\"office_run\">\n\
             {\"app\": \"word\", \"script\": \"tell application \\\"Microsoft Word\\\"\\n  activate\\nend tell\"}\n\
             </tool_call>]"
                .to_string(),
        );
    }
    // Match <|tool_call|> or similar model-specific markers without proper JSON
    if content.contains("<|tool_call|>") && !content.contains("<tool_call name=") {
        return Some(
            "[System: Your last message contained a malformed tool call marker. \
             Please use the correct format: \
             <tool_call name=\"tool_name\">{\"arg\": \"value\"}</tool_call>]"
                .to_string(),
        );
    }
    None
}

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
        .route("/{id}/stop", axum::routing::post(stop_processing))
        .route("/{id}/events", get(subscribe_events))
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

/// Fire-and-forget message send (ADR-019).
/// Stores the user message, spawns a background task for agent response,
/// and returns immediately with the stored user message.
async fn send_message(
    State(state): State<AppState>,
    Path(conv_id): Path<String>,
    Json(req): Json<SendMessageInput>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let mgr = ConversationManager::new(state.db.clone());

    // Store user message as unprocessed
    let user_msg = mgr
        .send_user_message(
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

    // Broadcast the user message to any connected event subscribers
    state.event_bus.send(
        &conv_id,
        xpressclaw_core::conversations::event_bus::ConversationEvent::Message {
            message: json!(user_msg),
        },
    );

    // Spawn background processor if not already running
    if !mgr.is_processing(&conv_id) {
        if let Some(llm_router) = state.llm_router() {
            let config = state.config();
            let agent_skills_map = config
                .agents
                .iter()
                .map(|a| {
                    let role =
                        append_skills(&a.full_system_prompt(), &a.skills, &state.config_path);
                    (a.name.clone(), role)
                })
                .collect();

            xpressclaw_core::conversations::processor::spawn(
                conv_id.clone(),
                xpressclaw_core::conversations::processor::ProcessorContext {
                    db: state.db.clone(),
                    config,
                    llm_router,
                    event_bus: state.event_bus.clone(),
                    rate_limiter: state.rate_limiter(),
                    agent_roles: agent_skills_map,
                    docker: state.docker().await,
                },
            );
        }
    }

    // Return immediately with the user message — agent response
    // will be processed in the background and delivered via SSE events.
    Ok((StatusCode::CREATED, Json(json!([user_msg]))))
}

/// Streaming version of send_message. Returns SSE events:
/// SSE event subscription for a conversation (ADR-019).
/// Replays messages after `after` ID, then streams live events.
#[derive(Deserialize)]
struct StopParams {
    agent_id: Option<String>,
}

async fn stop_processing(
    State(state): State<AppState>,
    Path(conv_id): Path<String>,
    Query(params): Query<StopParams>,
) -> StatusCode {
    let mgr = ConversationManager::new(state.db.clone());
    let _ = mgr.set_processing_status(&conv_id, "idle");
    let _ = mgr.mark_processed(&conv_id);

    // Send cancel to the specific agent (or all if none specified).
    let registry = xpressclaw_core::agents::registry::AgentRegistry::new(state.db.clone());
    let target_agents = if let Some(ref aid) = params.agent_id {
        vec![aid.clone()]
    } else {
        mgr.resolve_target_agents(&conv_id, "").unwrap_or_default()
    };

    for agent_id in &target_agents {
        if let Ok(agent) = registry.get(agent_id) {
            if agent.container_id.is_some() {
                if let Some(harness) = state.harness().await {
                    if let Some(port) = harness.endpoint_port(agent_id).await {
                        let client = xpressclaw_core::agents::harness::HarnessClient::new(port);
                        let _ = client.cancel().await;
                        tracing::info!(agent_id, "sent cancel to harness");
                    }
                }
            }
        }
    }

    tracing::info!(conv_id, "processing stopped by user");
    StatusCode::OK
}

/// The client can disconnect and reconnect — missed messages are
/// replayed from the DB on reconnect.
#[derive(Deserialize)]
struct EventsQuery {
    after: Option<i64>,
}

async fn subscribe_events(
    State(state): State<AppState>,
    Path(conv_id): Path<String>,
    Query(query): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let after_id = query.after.unwrap_or(0);
    let db = state.db.clone();
    let event_bus = state.event_bus.clone();

    let stream = async_stream::stream! {
        // Replay missed messages from DB
        let mgr = ConversationManager::new(db.clone());
        if let Ok(missed) = mgr.get_messages_after(&conv_id, after_id) {
            for msg in missed {
                if let Ok(evt) = Event::default().event("agent_message").json_data(json!(msg)) {
                    yield Ok(evt);
                }
            }
        }

        // Subscribe to live events
        let mut rx = event_bus.subscribe(&conv_id);
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let event_type = match &event {
                        xpressclaw_core::conversations::event_bus::ConversationEvent::Thinking { .. } => "thinking",
                        xpressclaw_core::conversations::event_bus::ConversationEvent::Chunk { .. } => "chunk",
                        xpressclaw_core::conversations::event_bus::ConversationEvent::Message { .. } => "agent_message",
                        xpressclaw_core::conversations::event_bus::ConversationEvent::Error { .. } => "error",
                        xpressclaw_core::conversations::event_bus::ConversationEvent::Done => "done",
                    };
                    if let Ok(data) = serde_json::to_string(&event) {
                        yield Ok(Event::default().event(event_type).data(data));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    // Slow consumer — send a hint to reconnect with after_id
                    tracing::warn!(conv_id = %conv_id, lagged = n, "SSE consumer lagged, events dropped");
                    if let Ok(evt) = Event::default().event("error").json_data(json!({
                        "type": "error",
                        "error": format!("Missed {n} events. Refresh to catch up."),
                    })) {
                        yield Ok(evt);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Streaming version of send_message (legacy — kept for backward compat).
/// Returns SSE events:
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
    let custom_pricing = config.llm.custom_pricing.clone();
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
        let budget_mgr = BudgetManager::new(db.clone(), config.clone());
        let cost_tracker = CostTracker::with_custom_pricing(db.clone(), &custom_pricing);

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
            let agent_cfg = config.agents.iter().find(|a| a.name == *agent_id);

            let model = agent_cfg
                .and_then(|c| c.model.as_deref())
                .map(String::from)
                .unwrap_or_else(|| {
                    llm_router
                        .models()
                        .first()
                        .map(|m| m.id.clone())
                        .unwrap_or_else(|| "local".to_string())
                });

            let base_role = agent_cfg
                .map(|c| c.role.as_str())
                .unwrap_or("You are a helpful AI assistant.");
            let agent_skills = agent_cfg.map(|c| &c.skills[..]).unwrap_or(&[]);
            let role = append_skills(base_role, agent_skills, &state.config_path);

            let history = mgr.get_messages(&conv_id, 20, None).unwrap_or_default();

            let mut llm_messages = vec![ChatMessage::text("system", &role)];
            for m in &history {
                let r = match m.sender_type.as_str() {
                    "agent" => "assistant",
                    _ => "user",
                };
                llm_messages.push(ChatMessage::text(r, &m.content));
            }

            let llm_req = ChatCompletionRequest {
                model: model.clone(),
                messages: llm_messages,
                temperature: Some(0.7),
                max_tokens: Some(4096),
                stream: Some(true),
                ..Default::default()
            };

            // Send "thinking" event
            if let Ok(evt) = Event::default().event("thinking").json_data(json!({
                "agent_id": agent_id
            })) {
                yield Ok(evt);
            }

            // Route through harness container if running, otherwise LLM router
            let stream_result = if agent.container_id.is_some() {
                match state.harness().await {
                    Some(h) => match h.endpoint_port(agent_id).await {
                        Some(port) => {
                            let harness = xpressclaw_core::agents::harness::HarnessClient::new(port);
                            harness.chat_stream(&llm_req).await
                        }
                        None => llm_router.chat_stream(&llm_req).await,
                    },
                    None => llm_router.chat_stream(&llm_req).await,
                }
            } else {
                llm_router.chat_stream(&llm_req).await
            };

            match stream_result {
                Ok(mut chunk_stream) => {
                    let mut full_content = String::new();
                    let mut total_tokens: i64 = 0;

                    while let Some(result) = chunk_stream.next().await {
                        match result {
                            Ok(chunk) => {
                                if let Some(choice) = chunk.choices.first() {
                                    if let Some(ref text) = choice.delta.content {
                                        full_content.push_str(text);
                                        total_tokens += text.len() as i64; // accumulate chars for estimation
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
                        let output_tokens = total_tokens / 4; // ~4 chars per token
                        let _ = cost_tracker.record(
                            agent_id, &model, input_tokens, output_tokens, "chat", Some(&conv_id),
                        );
                        let cost = cost_tracker.pricing().calculate(&model, input_tokens, output_tokens, 0, 0);
                        let _ = budget_mgr.update_spending(agent_id, cost);
                        rate_limiter.record_request(agent_id, (input_tokens + output_tokens) as u32);

                        // Check for task continuation signal
                        let (clean_content, task_signal) = extract_task_signal(&full_content);

                        if let Ok(agent_msg) = mgr.send_message(
                            &conv_id,
                            &SendMessage {
                                sender_type: "agent".into(),
                                sender_id: agent_id.clone(),
                                sender_name: Some(agent_id.clone()),
                                content: clean_content,
                                message_type: None,
                            },
                        ) {
                            if let Ok(evt) = Event::default().event("agent_message").json_data(json!(agent_msg)) {
                                yield Ok(evt);
                            }
                        }

                        // Detect malformed tool calls (e.g. XML-style <parameter=...>)
                        // and inject a hint so the model can correct itself next turn.
                        if let Some(hint) = detect_malformed_tool_call(&full_content) {
                            if let Ok(hint_msg) = mgr.send_message(
                                &conv_id,
                                &SendMessage {
                                    sender_type: "system".into(),
                                    sender_id: "xpressclaw".into(),
                                    sender_name: Some("system".into()),
                                    content: hint,
                                    message_type: None,
                                },
                            ) {
                                if let Ok(evt) = Event::default().event("agent_message").json_data(json!(hint_msg)) {
                                    yield Ok(evt);
                                }
                            }
                        }

                        // Create background task if continuation was signaled
                        if let Some(task_req) = task_signal {
                            let board = TaskBoard::new(db.clone());
                            if let Ok(task) = board.create(&CreateTask {
                                title: task_req.title.clone(),
                                description: task_req.description,
                                agent_id: Some(agent_id.clone()),
                                parent_task_id: None,
                                sop_id: None,
                                conversation_id: Some(conv_id.clone()),
                                priority: task_req.priority,
                                context: None,
                            }) {
                                // Enqueue for dispatcher
                                let queue = TaskQueue::new(db.clone());
                                let _ = queue.enqueue(&task.id, agent_id);

                                // Send task_status message to conversation
                                if let Ok(task_msg) = mgr.send_message(
                                    &conv_id,
                                    &SendMessage {
                                        sender_type: "system".into(),
                                        sender_id: agent_id.clone(),
                                        sender_name: Some(agent_id.clone()),
                                        content: json!({
                                            "task_id": task.id,
                                            "title": task_req.title,
                                            "status": "pending"
                                        }).to_string(),
                                        message_type: Some("task_status".into()),
                                    },
                                ) {
                                    if let Ok(evt) = Event::default().event("agent_message").json_data(json!(task_msg)) {
                                        yield Ok(evt);
                                    }
                                }
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

/// Append skill content directly to the agent's system prompt.
/// Critical skills (like build-app) are injected in full so the agent
/// doesn't need to call read_skill first.
fn append_skills(
    base_role: &str,
    agent_skills: &[String],
    config_path: &std::path::Path,
) -> String {
    if agent_skills.is_empty() {
        return base_role.to_string();
    }

    let skills_dir = config_path
        .parent()
        .map(|d| d.join("skills"))
        .unwrap_or_default();

    if !skills_dir.is_dir() {
        return base_role.to_string();
    }

    let mut sections = Vec::new();

    if let Ok(dirs) = std::fs::read_dir(&skills_dir) {
        for entry in dirs.flatten() {
            let skill_file = entry.path().join("SKILL.md");
            if !skill_file.is_file() {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&skill_file) {
                if !content.starts_with("---") {
                    continue;
                }
                let parts: Vec<&str> = content.splitn(3, "---").collect();
                if parts.len() < 3 {
                    continue;
                }
                let fm = parts[1];
                let body = parts[2].trim();
                let mut name = String::new();
                for line in fm.lines() {
                    if let Some(v) = line.strip_prefix("name:") {
                        name = v.trim().to_string();
                    }
                }
                if !name.is_empty() && agent_skills.contains(&name) {
                    sections.push(body.to_string());
                }
            }
        }
    }

    if sections.is_empty() {
        return base_role.to_string();
    }

    format!("{base_role}\n\n{}", sections.join("\n\n---\n\n"))
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
