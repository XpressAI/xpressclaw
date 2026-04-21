//! [`EchoHarness`] — a real harness that can respond to chat
//! completions, proving the end-to-end conversation flow during the
//! ADR-023 spike.
//!
//! Binds a per-agent TCP listener and serves OpenAI-compatible
//! `/v1/chat/completions` requests. Forwards each request through the
//! xpressclaw [`LlmRouter`](xpressclaw_core::llm::router::LlmRouter) —
//! so responses stream back from whatever provider the agent is
//! configured to use (cloud or local), with a pinned system-prompt
//! prefix that identifies the harness so its output is distinguishable
//! from a direct LLM-router fallback.
//!
//! This isn't a WASM workload — the "real harness running in a c2w
//! WASM sandbox" case needs either a preview-2 switch (TCP via
//! `wasi:sockets`) or a c2w-compiled image to exist. Both are follow-
//! up work. EchoHarness exists so the desktop app demonstrates the
//! agent → harness → LLM → response loop honestly *today*, while the
//! WASM parts of ADR-023 catch up.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use axum::extract::State as AxumState;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::post;
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::sync::RwLock as TokioRwLock;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use xpressclaw_core::error::{Error, Result};
use xpressclaw_core::harness::types::{ContainerInfo, ContainerSpec};
use xpressclaw_core::harness::Harness;
use xpressclaw_core::llm::router::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, LlmRouter, ToolCall,
};
use xpressclaw_core::tools::mcp::McpContent;
use xpressclaw_core::tools::mcp_manager::McpManager;

/// System-prompt prefix prepended to every request so responses are
/// visibly "from the harness" rather than a direct LLM fallback.
const HARNESS_BANNER: &str = "[xpressclaw echo harness: running in-process on the host. \
     This message is prepended so you can see responses flow through \
     the harness, not directly from the LLM router. Replace me with \
     a real pi/codex/opencode harness once the c2w images land.] ";

struct RunningAgent {
    port: u16,
    started_at: Instant,
    shutdown: CancellationToken,
    server_task: JoinHandle<()>,
}

/// Shared handler state — the LLM router (read-through so config
/// reloads land) and the MCP tool manager (so the agent loop can
/// expose tools and execute them).
#[derive(Clone)]
pub struct EchoHandlerState {
    pub router: Arc<RwLock<Option<Arc<LlmRouter>>>>,
    pub mcp_manager: Arc<McpManager>,
}

/// Upper bound on the agent loop's tool-call rounds per request.
/// Prevents runaway loops when the model repeatedly asks for tools
/// without ever returning a final answer. Generous for normal
/// multi-step tasks, tight enough to fail fast on pathological cases.
const MAX_TOOL_TURNS: usize = 20;

/// In-process harness that binds per-agent TCP listeners and serves
/// OpenAI-compatible `/v1/chat/completions`.
pub struct EchoHarness {
    state: EchoHandlerState,
    agents: Arc<TokioRwLock<HashMap<String, RunningAgent>>>,
}

impl EchoHarness {
    pub fn new(router: Arc<RwLock<Option<Arc<LlmRouter>>>>, mcp_manager: Arc<McpManager>) -> Self {
        Self {
            state: EchoHandlerState {
                router,
                mcp_manager,
            },
            agents: Arc::new(TokioRwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Harness for EchoHarness {
    fn kind(&self) -> &'static str {
        "echo"
    }

    async fn launch(&self, agent_id: &str, _spec: &ContainerSpec) -> Result<ContainerInfo> {
        // Already running? Report the existing record.
        {
            let agents = self.agents.read().await;
            if let Some(rec) = agents.get(agent_id) {
                return Ok(ContainerInfo {
                    container_id: format!("echo-{agent_id}"),
                    agent_id: agent_id.to_string(),
                    status: "running".to_string(),
                    host_port: Some(rec.port),
                });
            }
        }

        // Bind on an OS-allocated port so multiple agents don't collide.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| Error::Container(format!("echo harness bind for {agent_id}: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::Container(format!("echo harness local_addr: {e}")))?
            .port();

        let shutdown = CancellationToken::new();
        let shutdown_inner = shutdown.clone();
        let app = Router::new()
            .route("/v1/chat/completions", post(chat_completions))
            .with_state(self.state.clone());

        let server_task = tokio::spawn(async move {
            let svc = axum::serve(listener, app);
            tokio::select! {
                r = svc => {
                    if let Err(e) = r {
                        warn!(error = %e, "echo harness server exited with error");
                    }
                }
                _ = shutdown_inner.cancelled() => {
                    // Listener dropped when this task returns.
                }
            }
        });

        self.agents.write().await.insert(
            agent_id.to_string(),
            RunningAgent {
                port,
                started_at: Instant::now(),
                shutdown,
                server_task,
            },
        );

        info!(agent_id, port, "echo harness agent listening");

        Ok(ContainerInfo {
            container_id: format!("echo-{agent_id}"),
            agent_id: agent_id.to_string(),
            status: "running".to_string(),
            host_port: Some(port),
        })
    }

    async fn stop(&self, agent_id: &str) -> Result<()> {
        let rec = self.agents.write().await.remove(agent_id);
        if let Some(rec) = rec {
            rec.shutdown.cancel();
            rec.server_task.abort();
            let _ = rec.server_task.await;
            info!(agent_id, "echo harness agent stopped");
        }
        Ok(())
    }

    async fn stop_all(&self) -> Result<()> {
        let ids: Vec<String> = self.agents.read().await.keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id).await;
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ContainerInfo>> {
        let agents = self.agents.read().await;
        Ok(agents
            .iter()
            .map(|(id, a)| ContainerInfo {
                container_id: format!("echo-{id}"),
                agent_id: id.clone(),
                status: if a.server_task.is_finished() {
                    "exited".to_string()
                } else {
                    "running".to_string()
                },
                host_port: Some(a.port),
            })
            .collect())
    }

    async fn logs(&self, _agent_id: &str, _tail: usize) -> Result<String> {
        // Echo harness runs in-process and logs via `tracing`; there's
        // no separate stdio to capture here. Callers that need guest
        // logs are using this harness for dev and will look at the
        // xpressclaw server's own logs.
        Ok(String::new())
    }

    async fn is_running(&self, agent_id: &str) -> bool {
        self.agents
            .read()
            .await
            .get(agent_id)
            .map(|a| !a.server_task.is_finished())
            .unwrap_or(false)
    }

    async fn uptime_secs(&self, agent_id: &str) -> u64 {
        self.agents
            .read()
            .await
            .get(agent_id)
            .map(|a| a.started_at.elapsed().as_secs())
            .unwrap_or(0)
    }

    async fn endpoint_port(&self, agent_id: &str) -> Option<u16> {
        self.agents.read().await.get(agent_id).map(|a| a.port)
    }

    async fn ensure_image(&self, _image: &str) -> Result<()> {
        Ok(())
    }

    async fn image_matches(&self, _agent_id: &str, _expected: &str) -> Result<bool> {
        // No real image; the harness is whatever this binary embeds.
        Ok(true)
    }
}

/// `POST /v1/chat/completions` on the per-agent listener.
///
/// Implements an actual agent loop: injects available MCP tools into
/// the request, calls the LLM, and — if the model returns tool calls
/// — executes them via the MCP manager, appends the results to the
/// message history, and re-queries. Loops until the LLM produces a
/// plain text response or [`MAX_TOOL_TURNS`] is reached.
///
/// The final turn's response is what gets streamed back to the caller
/// (or returned as JSON in non-streaming mode). Intermediate
/// tool-using turns happen server-side without being surfaced as chat
/// messages — the user sees the final answer with tools having been
/// invoked. This matches the Docker-era claude-agent-sdk behavior.
async fn chat_completions(
    AxumState(state): AxumState<EchoHandlerState>,
    Json(mut req): Json<ChatCompletionRequest>,
) -> axum::response::Response {
    banner_prepend_system(&mut req);

    let router = match state.router.read().unwrap().clone() {
        Some(r) => r,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "error": "llm router not configured on host"
                })),
            )
                .into_response();
        }
    };

    // Inject MCP tools so the LLM can actually call them. Caller-
    // provided tools win — if a caller already specified tools (as a
    // claude-agent-sdk harness might), don't override.
    if req.tools.is_none() {
        let schemas = state.mcp_manager.tool_schemas().await;
        if !schemas.is_empty() {
            req.tools = Some(schemas);
        }
    }

    let streaming = req.stream.unwrap_or(false);

    // Agent loop: run tool-using turns to completion, stream the final
    // turn. If the first turn has no tool calls at all, the loop is a
    // no-op and we stream that turn directly.
    let mut working = req.clone();
    working.stream = Some(false); // non-streaming for tool-use turns
    for _ in 0..MAX_TOOL_TURNS {
        let resp = match router.chat(&working).await {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
        };

        let tool_calls = resp
            .choices
            .first()
            .and_then(|c| c.message.tool_calls.clone())
            .unwrap_or_default();

        if tool_calls.is_empty() {
            // Terminal turn. Either stream it now (if caller asked for
            // streaming) or return the JSON we already have.
            if streaming {
                // Re-run the same conversation in streaming mode so the
                // caller gets token-by-token output instead of a
                // one-shot chunk.
                let mut final_req = working.clone();
                final_req.stream = Some(true);
                let chunks = match router.chat_stream(&final_req).await {
                    Ok(s) => s,
                    Err(e) => {
                        return (
                            StatusCode::BAD_GATEWAY,
                            Json(serde_json::json!({ "error": e.to_string() })),
                        )
                            .into_response();
                    }
                };
                return stream_response(chunks).into_response();
            } else {
                return Json::<ChatCompletionResponse>(resp).into_response();
            }
        }

        // Append the assistant message (with tool_calls) to history
        // so the LLM sees its prior commitment in the next turn.
        if let Some(choice) = resp.choices.first() {
            working.messages.push(choice.message.clone());
        }

        // Execute each tool call and append results as tool messages.
        for tc in &tool_calls {
            let args: serde_json::Value =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({}));
            let tool_msg = match state.mcp_manager.call_tool(&tc.function.name, args).await {
                Ok(result) => format_tool_result(&result),
                Err(e) => format!("[tool error: {e}]"),
            };
            working
                .messages
                .push(ChatMessage::tool_result(tc.id.clone(), tool_msg));
        }
    }

    // Loop exhausted — return a best-effort error response.
    (
        StatusCode::INSUFFICIENT_STORAGE,
        Json(serde_json::json!({
            "error": format!(
                "agent loop exceeded {MAX_TOOL_TURNS} tool-call turns; \
                 the model is stuck in a tool-calling loop"
            ),
        })),
    )
        .into_response()
}

/// Flatten an MCP tool result's `content` blocks into a single string
/// suitable for a `tool` role message. Text blocks concatenate;
/// non-text blocks get a readable stand-in.
fn format_tool_result(result: &xpressclaw_core::tools::mcp::McpToolResult) -> String {
    let mut out = String::new();
    for block in &result.content {
        match block {
            McpContent::Text { text } => {
                out.push_str(text);
                out.push('\n');
            }
            McpContent::Image { mime_type, .. } => {
                out.push_str(&format!("[image: {mime_type}]\n"));
            }
            McpContent::Resource { uri, text } => {
                if let Some(t) = text {
                    out.push_str(t);
                    out.push('\n');
                } else {
                    out.push_str(&format!("[resource: {uri}]\n"));
                }
            }
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        if result.is_error {
            "[tool returned empty error]".to_string()
        } else {
            "[tool returned no content]".to_string()
        }
    } else {
        trimmed.to_string()
    }
}

// Silence the unused-import warning if ToolCall becomes unused in
// some conditional path; keeps the public symbol visible.
#[allow(dead_code)]
fn _assert_tool_call_usable(_t: &ToolCall) {}

fn banner_prepend_system(req: &mut ChatCompletionRequest) {
    if let Some(first) = req.messages.first_mut() {
        if first.role == "system" {
            first.content = format!("{HARNESS_BANNER}{}", first.content);
            return;
        }
    }
    req.messages
        .insert(0, ChatMessage::text("system", HARNESS_BANNER));
}

fn stream_response(
    chunks: xpressclaw_core::llm::router::ChatStream,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, std::convert::Infallible>>> {
    use futures_util::StreamExt;
    let stream = async_stream::stream! {
        futures_util::pin_mut!(chunks);
        while let Some(c) = chunks.next().await {
            match c {
                Ok(chunk) => {
                    let body = serde_json::to_string(&chunk).unwrap_or_default();
                    yield Ok::<_, std::convert::Infallible>(Event::default().data(body));
                }
                Err(e) => {
                    warn!(error = %e, "echo harness stream error");
                    break;
                }
            }
        }
        yield Ok(Event::default().data("[DONE]"));
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Convenience so callers can `use axum::response::IntoResponse` via
/// this re-export without adding another `use`.
use axum::response::IntoResponse;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn banner_prepends_when_no_system_message() {
        let mut req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            ..Default::default()
        };
        banner_prepend_system(&mut req);
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "system");
        assert!(req.messages[0].content.contains("xpressclaw echo harness"));
    }

    #[tokio::test]
    async fn banner_prepends_inside_existing_system_message() {
        let mut req = ChatCompletionRequest {
            model: "test".into(),
            messages: vec![
                ChatMessage::text("system", "existing role prompt"),
                ChatMessage::text("user", "hi"),
            ],
            ..Default::default()
        };
        banner_prepend_system(&mut req);
        assert_eq!(req.messages.len(), 2);
        assert!(req.messages[0].content.starts_with("[xpressclaw"));
        assert!(req.messages[0].content.contains("existing role prompt"));
    }

    #[tokio::test]
    async fn lifecycle_launch_list_stop() {
        let router_slot: Arc<RwLock<Option<Arc<LlmRouter>>>> = Arc::new(RwLock::new(None));
        let harness = EchoHarness::new(router_slot, Arc::new(McpManager::new()));
        let spec = ContainerSpec::default();

        let info = harness.launch("test-agent", &spec).await.expect("launch");
        assert_eq!(info.agent_id, "test-agent");
        assert!(info.host_port.is_some());
        assert!(harness.is_running("test-agent").await);
        assert_eq!(harness.list().await.unwrap().len(), 1);

        harness.stop("test-agent").await.expect("stop");
        assert!(!harness.is_running("test-agent").await);
        assert_eq!(harness.list().await.unwrap().len(), 0);
    }
}
