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
    Json(req): Json<ChatCompletionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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

    let model = req.model.clone();
    let streaming = req.stream.unwrap_or(false);

    debug!(model = %model, streaming, "anthropic messages request");

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
        Ok(anthropic_live_streaming_response(model, chunk_stream).into_response())
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

        let anthropic_resp = openai_to_anthropic_response(response);
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

        // content_block_start for index 0 (text)
        yield Ok(Event::default().event("content_block_start").data(
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }).to_string()
        ));

        let mut output_tokens = 0u64;

        futures_util::pin_mut!(chunk_stream);
        while let Some(chunk_result) = chunk_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    for choice in &chunk.choices {
                        if let Some(ref text) = choice.delta.content {
                            if !text.is_empty() {
                                output_tokens += 1;
                                let delta = json!({
                                    "type": "content_block_delta",
                                    "index": 0,
                                    "delta": { "type": "text_delta", "text": text }
                                });
                                yield Ok(Event::default().event("content_block_delta").data(delta.to_string()));
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "stream chunk error");
                    break;
                }
            }
        }

        // content_block_stop
        yield Ok(Event::default().event("content_block_stop").data(
            json!({ "type": "content_block_stop", "index": 0 }).to_string()
        ));

        // message_delta
        yield Ok(Event::default().event("message_delta").data(
            json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn", "stop_sequence": null },
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
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
        stream: Some(false), // always non-streaming internally
        top_p: req.top_p,
        stop: req.stop_sequences,
        tools,
        tool_choice,
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
fn openai_to_anthropic_response(resp: ChatCompletionResponse) -> AnthropicMessagesResponse {
    let mut content = Vec::new();

    if let Some(choice) = resp.choices.first() {
        // Reasoning/thinking content (from reasoning models like o1, Qwen3.5)
        if let Some(ref reasoning) = choice.message.reasoning_content {
            if !reasoning.is_empty() {
                content.push(json!({
                    "type": "thinking",
                    "thinking": reasoning,
                }));
            }
        }

        // Text content
        if !choice.message.content.is_empty() {
            content.push(json!({
                "type": "text",
                "text": choice.message.content,
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
                    "name": tc.function.name,
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
