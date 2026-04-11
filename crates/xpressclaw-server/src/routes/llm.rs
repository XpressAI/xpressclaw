use std::collections::HashMap;
use std::convert::Infallible;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::debug;

use xpressclaw_core::llm::router::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ToolCall, ToolCallFunction,
};

use crate::state::AppState;

/// Routes for the built-in LLM router.
/// Mounted at /v1/ — serves both OpenAI-compatible and Anthropic-compatible endpoints.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/chat/completions", post(chat_completions))
        .route("/messages", post(anthropic_messages))
        .route("/models", get(list_models))
}

// ---------------------------------------------------------------------------
// OpenAI-compatible endpoint
// ---------------------------------------------------------------------------

async fn chat_completions(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(mut req): Json<ChatCompletionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Check for budget degraded model override via API key
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(agent_id) = auth
        .strip_prefix("Bearer sk-ant-")
        .or_else(|| auth.strip_prefix("Bearer sk-"))
    {
        // Only check placeholder keys (real keys won't start with sk-ant- or sk-)
        if !agent_id.contains("xpressclaw") {
            let budget_mgr = xpressclaw_core::budget::manager::BudgetManager::new(
                state.db.clone(),
                state.config(),
            );
            if let Ok(Some(fallback)) = budget_mgr.degraded_model(agent_id) {
                debug!(
                    agent_id,
                    original_model = %req.model,
                    fallback_model = %fallback,
                    "budget degraded: switching model"
                );
                req.model = fallback;
            }
        }
    }

    let router = state.llm_router().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "LLM router not configured" })),
        )
    })?;

    let response = router.chat(&req).await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!(response)))
}

async fn list_models(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let router = state.llm_router().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "LLM router not configured" })),
        )
    })?;

    let models = router.models();
    Ok(Json(json!({ "object": "list", "data": models })))
}

// ---------------------------------------------------------------------------
// Anthropic Messages API compatible endpoint
// ---------------------------------------------------------------------------

/// Anthropic Messages API request (inbound).
#[derive(Debug, Deserialize)]
struct AnthropicMessagesRequest {
    model: String,
    messages: Vec<AnthropicInboundMessage>,
    max_tokens: i64,
    #[serde(default)]
    system: Option<Value>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    stop_sequences: Option<Vec<String>>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    tools: Option<Vec<Value>>,
    #[serde(default)]
    tool_choice: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicInboundMessage {
    role: String,
    content: Value, // string or array of content blocks
}

/// Anthropic Messages API response (outbound).
#[derive(Debug, Serialize)]
struct AnthropicMessagesResponse {
    id: String,
    #[serde(rename = "type")]
    response_type: String,
    role: String,
    content: Vec<Value>,
    model: String,
    stop_reason: Option<String>,
    stop_sequence: Option<String>,
    usage: AnthropicUsageOut,
}

#[derive(Debug, Clone, Serialize)]
struct AnthropicUsageOut {
    input_tokens: i64,
    output_tokens: i64,
}

/// Handle Anthropic Messages API requests.
///
/// Anthropic Messages API endpoint.
///
/// For Claude models: proxies directly to the Anthropic API, preserving the full
/// request (tools, tool_use, tool_result, streaming) without lossy conversion.
/// For non-Claude models: converts Anthropic→OpenAI, routes through LLM router,
/// converts response back.
async fn anthropic_messages(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let req: AnthropicMessagesRequest =
        serde_json::from_slice(&body_bytes).map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "type": "error", "error": { "type": "invalid_request", "message": e.to_string() } })),
            )
        })?;

    let mut model = req.model.clone();
    let streaming = req.stream.unwrap_or(false);

    // Check if this agent has a degraded model override (budget degrade action).
    // Agent ID is encoded in the placeholder API key: "sk-ant-{agent_name}".
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(agent_id) = api_key.strip_prefix("sk-ant-") {
        let budget_mgr =
            xpressclaw_core::budget::manager::BudgetManager::new(state.db.clone(), state.config());
        if let Ok(Some(fallback)) = budget_mgr.degraded_model(agent_id) {
            debug!(
                agent_id,
                original_model = %model,
                fallback_model = %fallback,
                "budget degraded: switching model"
            );
            model = fallback;
        }
    }

    let num_tools = req.tools.as_ref().map(|t| t.len()).unwrap_or(0);
    let num_messages = req.messages.len();
    let has_tool_choice = req.tool_choice.is_some();
    debug!(
        model = %model,
        streaming,
        num_tools,
        num_messages,
        has_tool_choice,
        "anthropic messages request"
    );
    if num_tools > 0 {
        if let Some(ref tools) = req.tools {
            for tool in tools {
                debug!(tool_name = ?tool.get("name"), "  tool");
            }
        }
    }

    // For Claude models with an Anthropic API key: proxy directly to Anthropic API.
    // This preserves tools, tool_use/tool_result blocks, and streaming without lossy conversion.
    let config = state.config();
    if model.starts_with("claude") {
        if let Some(ref api_key) = config.llm.anthropic_api_key {
            return proxy_to_anthropic(api_key, &body_bytes, streaming, &headers).await;
        }
    }

    // For non-Claude models: convert Anthropic→OpenAI→LLM router→Anthropic
    let router = state.llm_router().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "type": "error",
                "error": { "type": "api_error", "message": "LLM router not configured" }
            })),
        )
    })?;

    // Build a lookup from unprefixed tool name → full MCP-prefixed name.
    // Small local models often drop the "mcp__server__" prefix when generating
    // tool calls, so we rewrite them in the response before the CLI sees them.
    let tool_name_map = build_tool_name_map(req.tools.as_deref());

    let openai_req = anthropic_to_openai_request(req);

    if streaming {
        let chunk_stream = router.chat_stream(&openai_req).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "type": "error",
                    "error": { "type": "api_error", "message": e.to_string() }
                })),
            )
        })?;
        Ok(anthropic_live_streaming_response(model, chunk_stream, tool_name_map).into_response())
    } else {
        let response = router.chat(&openai_req).await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "type": "error",
                    "error": { "type": "api_error", "message": e.to_string() }
                })),
            )
        })?;

        let anthropic_resp = openai_to_anthropic_response(response, &tool_name_map);
        let body = serde_json::to_value(&anthropic_resp).unwrap_or(json!({}));
        Ok(Json(body).into_response())
    }
}

/// Proxy an Anthropic Messages API request directly to api.anthropic.com.
/// Preserves the full request body (tools, tool_use, tool_result, streaming) without conversion.
async fn proxy_to_anthropic(
    api_key: &str,
    body: &[u8],
    streaming: bool,
    client_headers: &axum::http::HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    let client = reqwest::Client::new();

    let mut req = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("content-type", "application/json");

    // Forward Anthropic-specific headers from the CLI (version, beta features, etc.)
    for key in [
        "anthropic-version",
        "anthropic-beta",
        "anthropic-dangerous-direct-browser-access",
    ] {
        if let Some(val) = client_headers.get(key) {
            req = req.header(key, val);
        }
    }
    // Default version if the client didn't send one
    if client_headers.get("anthropic-version").is_none() {
        req = req.header("anthropic-version", "2023-06-01");
    }

    let resp = req.body(body.to_vec()).send().await.map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "type": "error",
                "error": { "type": "api_error", "message": format!("Anthropic proxy error: {e}") }
            })),
        )
    })?;

    let status =
        axum::http::StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);

    if streaming {
        // Stream the SSE response directly through to the client
        let byte_stream = resp.bytes_stream();
        let body = axum::body::Body::from_stream(byte_stream);
        Ok(axum::response::Response::builder()
            .status(status)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .body(body)
            .unwrap())
    } else {
        let response_bytes = resp.bytes().await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "type": "error",
                    "error": { "type": "api_error", "message": format!("Failed to read Anthropic response: {e}") }
                })),
            )
        })?;
        Ok(axum::response::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(response_bytes))
            .unwrap())
    }
}

/// True streaming: convert OpenAI chat completion chunks into Anthropic SSE events.
///
/// Each OpenAI chunk with text content becomes an Anthropic `content_block_delta`.
/// This allows the Claude CLI to receive tokens as they're generated, preventing timeouts.
fn anthropic_live_streaming_response(
    model: String,
    chunk_stream: xpressclaw_core::llm::router::ChatStream,
    tool_name_map: HashMap<String, String>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = async_stream::stream! {
        use futures_util::StreamExt;

        let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());

        // message_start
        let message_start = json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        });
        yield Ok(Event::default().event("message_start").data(message_start.to_string()));

        let mut output_tokens = 0u64;
        let mut block_index: i64 = 0;
        let mut thinking_block_open = false;
        let mut text_block_open = false;
        let mut has_tool_calls = false;

        // Track streaming tool call assembly (OpenAI sends them incrementally)
        struct ToolCallState {
            id: String,
            name: String,
            arguments: String,
            block_index: i64,
        }
        let mut tool_calls: std::collections::HashMap<i64, ToolCallState> = std::collections::HashMap::new();

        futures_util::pin_mut!(chunk_stream);
        while let Some(chunk_result) = chunk_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    for choice in &chunk.choices {
                        // Reasoning content → thinking block (separate from text)
                        if let Some(ref reasoning) = choice.delta.reasoning_content {
                            if !reasoning.is_empty() {
                                if !thinking_block_open {
                                    yield Ok(Event::default().event("content_block_start").data(
                                        json!({ "type": "content_block_start", "index": block_index, "content_block": { "type": "thinking", "thinking": "" } }).to_string()
                                    ));
                                    thinking_block_open = true;
                                }
                                output_tokens += 1;
                                yield Ok(Event::default().event("content_block_delta").data(
                                    json!({ "type": "content_block_delta", "index": block_index, "delta": { "type": "thinking_delta", "thinking": reasoning } }).to_string()
                                ));
                            }
                        }

                        // Text content → text block
                        if let Some(ref text) = choice.delta.content {
                            if !text.is_empty() {
                                // Close thinking block first if transitioning
                                if thinking_block_open {
                                    yield Ok(Event::default().event("content_block_stop").data(
                                        json!({ "type": "content_block_stop", "index": block_index }).to_string()
                                    ));
                                    thinking_block_open = false;
                                    block_index += 1;
                                }
                                if !text_block_open {
                                    yield Ok(Event::default().event("content_block_start").data(
                                        json!({ "type": "content_block_start", "index": block_index, "content_block": { "type": "text", "text": "" } }).to_string()
                                    ));
                                    text_block_open = true;
                                }
                                output_tokens += 1;
                                yield Ok(Event::default().event("content_block_delta").data(
                                    json!({ "type": "content_block_delta", "index": block_index, "delta": { "type": "text_delta", "text": text } }).to_string()
                                ));
                            }
                        }

                        // Tool call deltas
                        if let Some(ref tcs) = choice.delta.tool_calls {
                            // Close any open blocks
                            if thinking_block_open {
                                yield Ok(Event::default().event("content_block_stop").data(
                                    json!({ "type": "content_block_stop", "index": block_index }).to_string()
                                ));
                                thinking_block_open = false;
                                block_index += 1;
                            }
                            if text_block_open {
                                yield Ok(Event::default().event("content_block_stop").data(
                                    json!({ "type": "content_block_stop", "index": block_index }).to_string()
                                ));
                                text_block_open = false;
                                block_index += 1;
                            }
                            has_tool_calls = true;

                            for tc in tcs {
                                let tc_index = tc.index;
                                let state = tool_calls.entry(tc_index).or_insert_with(|| {
                                    let bi = block_index + tc_index;
                                    ToolCallState {
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                        block_index: bi,
                                    }
                                });

                                if let Some(ref id) = tc.id {
                                    state.id = id.clone();
                                }
                                if let Some(ref func) = tc.function {
                                    if let Some(ref name) = func.name {
                                        state.name = tool_name_map.get(name.as_str()).cloned().unwrap_or_else(|| name.clone());
                                        // Emit content_block_start for this tool_use
                                        yield Ok(Event::default().event("content_block_start").data(
                                            json!({
                                                "type": "content_block_start",
                                                "index": state.block_index,
                                                "content_block": {
                                                    "type": "tool_use",
                                                    "id": state.id,
                                                    "name": state.name,
                                                    "input": {}
                                                }
                                            }).to_string()
                                        ));
                                    }
                                    if let Some(ref args) = func.arguments {
                                        state.arguments.push_str(args);
                                        // Emit input_json_delta
                                        yield Ok(Event::default().event("content_block_delta").data(
                                            json!({
                                                "type": "content_block_delta",
                                                "index": state.block_index,
                                                "delta": { "type": "input_json_delta", "partial_json": args }
                                            }).to_string()
                                        ));
                                    }
                                }
                            }
                        }

                        // Check finish_reason for tool_calls
                        if choice.finish_reason.as_deref() == Some("tool_calls") {
                            has_tool_calls = true;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "stream chunk error");
                    break;
                }
            }
        }

        // Close any open blocks
        if thinking_block_open {
            yield Ok(Event::default().event("content_block_stop").data(
                json!({ "type": "content_block_stop", "index": block_index }).to_string()
            ));
            block_index += 1;
        }
        if text_block_open {
            yield Ok(Event::default().event("content_block_stop").data(
                json!({ "type": "content_block_stop", "index": block_index }).to_string()
            ));
            block_index += 1;
        }

        // Close tool_use blocks
        for state in tool_calls.values() {
            yield Ok(Event::default().event("content_block_stop").data(
                json!({ "type": "content_block_stop", "index": state.block_index }).to_string()
            ));
        }

        // If no blocks were emitted at all, emit an empty text block
        if block_index == 0 && tool_calls.is_empty() && !thinking_block_open && !text_block_open {
            yield Ok(Event::default().event("content_block_start").data(
                json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }).to_string()
            ));
            yield Ok(Event::default().event("content_block_stop").data(
                json!({ "type": "content_block_stop", "index": 0 }).to_string()
            ));
        }

        let stop_reason = if has_tool_calls { "tool_use" } else { "end_turn" };

        // message_delta
        yield Ok(Event::default().event("message_delta").data(
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": output_tokens }
            }).to_string()
        ));

        // message_stop
        yield Ok(Event::default().event("message_stop").data(
            json!({ "type": "message_stop" }).to_string()
        ));
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// Tool name rewriting for local models
// ---------------------------------------------------------------------------

/// Build a map from unprefixed tool names to their full MCP-prefixed names.
///
/// Local models often generate tool calls with short names (e.g. `publish_app`)
/// instead of the full MCP-prefixed name the Claude CLI expects
/// (e.g. `mcp__xpressclaw__publish_app`). This map lets us rewrite them.
fn build_tool_name_map(tools: Option<&[Value]>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(tools) = tools else { return map };
    for tool in tools {
        let Some(full_name) = tool.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        // Only care about MCP-prefixed names (contain "__")
        if let Some(base) = full_name.rsplit("__").next() {
            if base != full_name {
                // If two tools share a base name, remove the entry to avoid
                // ambiguous rewrites — let the CLI report the error instead.
                if map.contains_key(base) {
                    map.remove(base);
                } else {
                    map.insert(base.to_string(), full_name.to_string());
                }
            }
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Conversion: Anthropic → OpenAI
// ---------------------------------------------------------------------------

/// Convert an Anthropic Messages API request into an OpenAI ChatCompletionRequest.
fn anthropic_to_openai_request(req: AnthropicMessagesRequest) -> ChatCompletionRequest {
    let mut messages = Vec::new();

    // System message (string or array of text blocks)
    if let Some(system) = req.system {
        let text = match system {
            Value::String(s) => s,
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type")?.as_str()? == "text" {
                        b.get("text")?.as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        };
        if !text.is_empty() {
            messages.push(ChatMessage::text("system", text));
        }
    }

    // Convert each message
    for msg in req.messages {
        match msg.content {
            Value::String(s) => {
                messages.push(ChatMessage::text(&msg.role, s));
            }
            Value::Array(blocks) => {
                convert_anthropic_content_blocks(&msg.role, &blocks, &mut messages);
            }
            _ => {
                messages.push(ChatMessage::text(&msg.role, ""));
            }
        }
    }

    // Convert tools: Anthropic {name, description, input_schema} → OpenAI {type, function}
    let tools = req.tools.map(|anthropic_tools| {
        anthropic_tools
            .into_iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                        "description": tool.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                        "parameters": tool.get("input_schema").cloned().unwrap_or(json!({})),
                    }
                })
            })
            .collect()
    });

    // Convert tool_choice: Anthropic {type: "auto"|"any"|"tool"} → OpenAI string
    let tool_choice = req.tool_choice.and_then(|tc| {
        let obj = tc.as_object()?;
        match obj.get("type")?.as_str()? {
            "auto" => Some("auto".to_string()),
            "any" => Some("required".to_string()),
            _ => Some("auto".to_string()),
        }
    });

    ChatCompletionRequest {
        model: req.model,
        messages,
        temperature: Some(req.temperature.unwrap_or(1.0)),
        max_tokens: Some(req.max_tokens),
        stream: Some(false), // always non-streaming internally
        top_p: req.top_p,
        stop: req.stop_sequences,
        tools,
        tool_choice,
        reasoning_budget: None,
    }
}

/// Parse an array of Anthropic content blocks into OpenAI ChatMessages.
///
/// Handles text, tool_use (→ assistant tool_calls), and tool_result (→ tool messages).
fn convert_anthropic_content_blocks(role: &str, blocks: &[Value], messages: &mut Vec<ChatMessage>) {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for block in blocks {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(json!({}));
                tool_calls.push(ToolCall {
                    id,
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name,
                        arguments: serde_json::to_string(&input).unwrap_or_default(),
                    },
                });
            }
            "tool_result" => {
                let tool_call_id = block
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = extract_tool_result_content(block);
                messages.push(ChatMessage::tool_result(tool_call_id, content));
            }
            _ => {}
        }
    }

    if !text_parts.is_empty() || !tool_calls.is_empty() {
        let content = text_parts.join("\n");
        let mut chat_msg = ChatMessage {
            role: role.to_string(),
            content,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            ..Default::default()
        };
        // If there's no text but there are tool calls, content can be empty
        if chat_msg.content.is_empty() && chat_msg.tool_calls.is_some() {
            chat_msg.content = String::new();
        }
        messages.push(chat_msg);
    }
}

/// Extract text content from a tool_result block.
/// The content field can be a string or an array of content blocks.
fn extract_tool_result_content(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type")?.as_str()? == "text" {
                    b.get("text")?.as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Conversion: OpenAI → Anthropic
// ---------------------------------------------------------------------------

/// Convert an OpenAI ChatCompletionResponse into an Anthropic Messages API response.
fn openai_to_anthropic_response(
    resp: ChatCompletionResponse,
    tool_name_map: &HashMap<String, String>,
) -> AnthropicMessagesResponse {
    let mut content = Vec::new();

    if let Some(choice) = resp.choices.first() {
        // Skip reasoning_content — the Claude CLI doesn't handle thinking blocks
        // from non-Claude models. If content is empty but reasoning exists, use
        // reasoning as the text content so the response isn't empty.
        let text_content = if choice.message.content.is_empty() {
            choice
                .message
                .reasoning_content
                .as_deref()
                .unwrap_or("")
                .to_string()
        } else {
            choice.message.content.clone()
        };

        // Text content
        if !text_content.is_empty() {
            content.push(json!({
                "type": "text",
                "text": text_content,
            }));
        }

        // Tool calls → tool_use blocks
        if let Some(ref tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                let input: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
                content.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tool_name_map.get(&tc.function.name).unwrap_or(&tc.function.name),
                    "input": input,
                }));
            }
        }
    }

    // Map finish_reason: OpenAI → Anthropic
    let stop_reason = resp
        .choices
        .first()
        .and_then(|c| c.finish_reason.as_ref())
        .map(|r| match r.as_str() {
            "stop" => "end_turn".to_string(),
            "length" => "max_tokens".to_string(),
            "tool_calls" => "tool_use".to_string(),
            other => other.to_string(),
        });

    let usage = resp
        .usage
        .as_ref()
        .map(|u| AnthropicUsageOut {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
        })
        .unwrap_or(AnthropicUsageOut {
            input_tokens: 0,
            output_tokens: 0,
        });

    AnthropicMessagesResponse {
        id: resp.id,
        response_type: "message".into(),
        role: "assistant".into(),
        content,
        model: resp.model,
        stop_reason,
        stop_sequence: None,
        usage,
    }
}

// Make axum response types work with our handler return type.
use axum::response::IntoResponse;

impl IntoResponse for AnthropicMessagesResponse {
    fn into_response(self) -> axum::response::Response {
        let body = serde_json::to_value(&self).unwrap_or(json!({}));
        Json(body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use xpressclaw_core::config::Config;
    use xpressclaw_core::db::Database;
    use xpressclaw_core::llm::openai::OpenAiProvider;
    use xpressclaw_core::llm::router::LlmRouter;

    use crate::state::AppState;

    async fn assert_ok(resp: axum::http::Response<Body>, context: &str) -> serde_json::Value {
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8_lossy(&bytes);
        assert_eq!(
            status, 200,
            "{context}: expected 200, got {status}. Body: {body_str}"
        );
        serde_json::from_slice(&bytes).expect("response should be valid JSON")
    }

    fn env_or_skip(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| {
            eprintln!("Skipping: {key} not set");
            String::new()
        })
    }

    fn test_app_with_openai() -> Option<Router> {
        let base_url = env_or_skip("OPENAI_BASE_URL");
        let api_key = env_or_skip("OPENAI_API_KEY");
        if base_url.is_empty() || api_key.is_empty() {
            return None;
        }

        let db = Arc::new(Database::open_memory().unwrap());
        let mut config = Config::default();
        config.llm.default_provider = "openai".into();
        config.llm.openai_api_key = Some(api_key.clone());
        config.llm.openai_base_url = Some(base_url.clone());
        let config = Arc::new(config);

        let provider = OpenAiProvider::new(Some(api_key), Some(base_url));
        let mut router = LlmRouter::new(&config.llm);
        router.register_provider("openai", Arc::new(provider));

        let state = AppState::new(
            config,
            db,
            Some(Arc::new(router)),
            std::path::PathBuf::from("test.yaml"),
            true,
        );

        Some(Router::new().nest("/v1", super::routes()).with_state(state))
    }

    /// Test OpenAI-compatible /v1/chat/completions endpoint.
    #[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
    #[tokio::test]
    async fn test_openai_chat_completions() {
        let app = match test_app_with_openai() {
            Some(a) => a,
            None => return,
        };

        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 500,
            "temperature": 0.1,
            "messages": [{"role": "user", "content": "Say hello in exactly 3 words."}]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = assert_ok(resp, "chat completions").await;
        let msg = &json["choices"][0]["message"];
        let content = msg["content"].as_str().unwrap_or("");
        let reasoning = msg["reasoning_content"].as_str().unwrap_or("");
        assert!(
            !content.is_empty() || !reasoning.is_empty(),
            "response should have content or reasoning_content, got: {}",
            serde_json::to_string_pretty(msg).unwrap()
        );
        eprintln!("Content: {content}");
        if !reasoning.is_empty() {
            eprintln!("Reasoning: {reasoning}");
        }
    }

    /// Test Anthropic-compatible /v1/messages endpoint with a non-Claude model
    /// (routes through OpenAI provider via conversion).
    #[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
    #[tokio::test]
    async fn test_anthropic_messages_via_openai() {
        let app = match test_app_with_openai() {
            Some(a) => a,
            None => return,
        };

        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 50,
            "temperature": 0.1,
            "messages": [{"role": "user", "content": "Say hello in exactly 3 words."}]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .header("anthropic-version", "2023-06-01")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = assert_ok(resp, "/v1/messages").await;
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "assistant");
        let content = json["content"].as_array().expect("content should be array");
        assert!(
            !content.is_empty(),
            "content should have at least one block"
        );
        eprintln!(
            "Response: {}",
            serde_json::to_string_pretty(&json["content"]).unwrap()
        );
    }

    /// Test /v1/messages with tool definitions (non-Claude model).
    #[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
    #[tokio::test]
    async fn test_anthropic_messages_with_tools() {
        let app = match test_app_with_openai() {
            Some(a) => a,
            None => return,
        };

        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 200,
            "temperature": 0.1,
            "tool_choice": {"type": "any"},
            "tools": [{
                "name": "get_weather",
                "description": "Get current weather for a city",
                "input_schema": {
                    "type": "object",
                    "properties": {"city": {"type": "string"}},
                    "required": ["city"]
                }
            }],
            "messages": [{"role": "user", "content": "What is the weather in Tokyo?"}]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .header("anthropic-version", "2023-06-01")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = assert_ok(resp, "/v1/messages with tools").await;
        assert_eq!(json["type"], "message");
        let content = json["content"].as_array().expect("content should be array");
        assert!(
            !content.is_empty(),
            "content should have at least one block"
        );
        eprintln!("Response: {}", serde_json::to_string_pretty(&json).unwrap());
    }

    /// Test /v1/messages with tool_choice=any forces tool use and returns tool_use blocks.
    #[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
    #[tokio::test]
    async fn test_anthropic_messages_forced_tool_call() {
        let app = match test_app_with_openai() {
            Some(a) => a,
            None => return,
        };

        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 2000,
            "temperature": 0.1,
            "tool_choice": {"type": "any"},
            "tools": [{
                "name": "Bash",
                "description": "Run a shell command and return the output",
                "input_schema": {
                    "type": "object",
                    "properties": {"command": {"type": "string", "description": "The command to run"}},
                    "required": ["command"]
                }
            }],
            "messages": [{"role": "user", "content": "List files in /workspace"}]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .header("anthropic-version", "2023-06-01")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let json = assert_ok(resp, "/v1/messages forced tool call").await;
        assert_eq!(json["type"], "message");
        let content = json["content"].as_array().expect("content should be array");
        assert!(
            !content.is_empty(),
            "content should have at least one block"
        );

        // Should have a tool_use block
        let has_tool_use = content.iter().any(|b| b["type"] == "tool_use");
        assert!(
            has_tool_use,
            "response should contain a tool_use block, got: {}",
            serde_json::to_string_pretty(&json["content"]).unwrap()
        );

        // Verify tool_use block structure
        let tool_block = content.iter().find(|b| b["type"] == "tool_use").unwrap();
        assert_eq!(tool_block["name"], "Bash");
        assert!(tool_block["id"].is_string(), "tool_use should have an id");
        assert!(
            tool_block["input"].is_object(),
            "tool_use should have input"
        );
        assert_eq!(json["stop_reason"], "tool_use");

        eprintln!(
            "Tool call: {}",
            serde_json::to_string_pretty(tool_block).unwrap()
        );
    }

    /// Test /v1/models endpoint returns models from the provider.
    #[ignore = "requires OPENAI_BASE_URL and OPENAI_API_KEY"]
    #[tokio::test]
    async fn test_list_models() {
        let app = match test_app_with_openai() {
            Some(a) => a,
            None => return,
        };

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["data"].is_array());
    }
}
