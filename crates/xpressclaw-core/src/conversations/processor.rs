//! Background conversation processor (ADR-019, ADR-021).
//!
//! Routes conversation messages through agent harness sessions.
//! The harness has the real MCP tools, correct personality, and
//! maintains context across messages via persistent sessions.
//!
//! Falls back to LLM router streaming when no harness is available.

use std::sync::Arc;

use serde_json::json;
use tracing::{debug, info, warn};

use crate::activity::ActivityManager;
use crate::agents::registry::AgentRegistry;
use crate::budget::manager::BudgetManager;
use crate::budget::rate_limiter::{RateLimitResult, RateLimiter};
use crate::budget::tracker::CostTracker;
use crate::config::Config;
use crate::conversations::event_bus::{ConversationEvent, ConversationEventBus};
use crate::conversations::{ConversationManager, SendMessage};
use crate::db::Database;
use crate::llm::router::{ChatCompletionRequest, ChatMessage, LlmRouter};
use crate::memory::hooks::{self, MemoryHooks};

use futures_util::StreamExt;

/// Context needed by the processor. Built by the caller and passed in.
pub struct ProcessorContext {
    pub db: Arc<Database>,
    pub config: Arc<Config>,
    pub llm_router: Arc<LlmRouter>,
    pub event_bus: Arc<ConversationEventBus>,
    pub rate_limiter: Arc<RateLimiter>,
    /// Pre-computed agent roles (agent_id → system prompt with skills injected).
    pub agent_roles: std::collections::HashMap<String, String>,
}

/// Spawn a background task to process agent responses for a conversation.
pub fn spawn(conv_id: String, ctx: ProcessorContext) {
    tokio::spawn(async move {
        process_loop(&conv_id, &ctx).await;
    });
}

/// Process all pending user messages in a conversation.
async fn process_loop(conv_id: &str, ctx: &ProcessorContext) {
    let mgr = ConversationManager::new(ctx.db.clone());

    let _ = mgr.set_processing_status(conv_id, "processing");

    loop {
        // Check if processing was cancelled (user hit stop)
        if !mgr.is_processing(conv_id) {
            info!(conv_id, "processing cancelled by user");
            break;
        }
        if !mgr.has_unprocessed(conv_id) {
            break;
        }

        let target_agents = match mgr.resolve_target_agents(conv_id, "") {
            Ok(agents) => agents,
            Err(e) => {
                warn!(conv_id, error = %e, "failed to resolve target agents");
                break;
            }
        };

        if target_agents.is_empty() {
            debug!(conv_id, "no target agents");
            break;
        }

        let _ = mgr.mark_processed(conv_id);

        let registry = AgentRegistry::new(ctx.db.clone());
        let budget_mgr = BudgetManager::new(ctx.db.clone(), ctx.config.clone());
        let custom_pricing = ctx.config.llm.custom_pricing.clone();
        let cost_tracker = CostTracker::with_custom_pricing(ctx.db.clone(), &custom_pricing);

        for agent_id in &target_agents {
            // Budget check
            match budget_mgr.check_budget(agent_id) {
                Ok(false) => {
                    ctx.event_bus.send(
                        conv_id,
                        ConversationEvent::Error {
                            agent_id: Some(agent_id.clone()),
                            error: format!("Budget exceeded for agent {agent_id}"),
                        },
                    );
                    continue;
                }
                Err(e) => {
                    ctx.event_bus.send(
                        conv_id,
                        ConversationEvent::Error {
                            agent_id: Some(agent_id.clone()),
                            error: e.to_string(),
                        },
                    );
                    continue;
                }
                Ok(true) => {}
            }

            // Rate limit check
            match ctx.rate_limiter.check(agent_id) {
                RateLimitResult::RequestsExceeded { limit, .. } => {
                    ctx.event_bus.send(
                        conv_id,
                        ConversationEvent::Error {
                            agent_id: Some(agent_id.clone()),
                            error: format!("Rate limit reached ({limit} requests/min)"),
                        },
                    );
                    continue;
                }
                RateLimitResult::TokensExceeded { limit, .. } => {
                    ctx.event_bus.send(
                        conv_id,
                        ConversationEvent::Error {
                            agent_id: Some(agent_id.clone()),
                            error: format!("Token rate limit reached ({limit} tokens/min)"),
                        },
                    );
                    continue;
                }
                RateLimitResult::Allowed => {}
            }

            // Verify agent exists
            if registry.get(agent_id).is_err() {
                continue;
            }
            let agent_cfg = ctx.config.agents.iter().find(|a| a.name == *agent_id);

            let model = agent_cfg
                .and_then(|c| c.model.as_deref())
                .map(String::from)
                .unwrap_or_else(|| {
                    ctx.llm_router
                        .models()
                        .first()
                        .map(|m| m.id.clone())
                        .unwrap_or_else(|| "local".to_string())
                });

            let role = ctx.agent_roles.get(agent_id).cloned().unwrap_or_else(|| {
                agent_cfg
                    .map(|c| c.full_system_prompt())
                    .unwrap_or_else(|| "You are a helpful AI assistant.".to_string())
            });

            // Get conversation history so the harness can restore context
            // after a container restart. We send the full history — the harness
            // only injects it when starting a fresh session.
            let history = mgr.get_messages(conv_id, 1000, None).unwrap_or_default();
            let last_user_msg = history
                .iter()
                .rev()
                .find(|m| m.sender_type == "user")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let sender_name = history
                .iter()
                .rev()
                .find(|m| m.sender_type == "user")
                .and_then(|m| m.sender_name.clone())
                .unwrap_or_else(|| "User".to_string());
            // Serialize history for the harness, excluding the current
            // message (which will be sent as a live query after session
            // reconstruction).
            let last_msg_id = history
                .iter()
                .rev()
                .find(|m| m.sender_type == "user" && m.content == last_user_msg)
                .map(|m| m.id);
            let history_json: serde_json::Value = history
                .iter()
                .filter(|m| Some(m.id) != last_msg_id)
                .map(|m| {
                    json!({
                        "sender_type": m.sender_type,
                        "sender_name": m.sender_name,
                        "content": m.content,
                    })
                })
                .collect();

            if last_user_msg.is_empty() {
                continue;
            }

            // Broadcast "thinking" event
            ctx.event_bus.send(
                conv_id,
                ConversationEvent::Thinking {
                    agent_id: agent_id.clone(),
                },
            );
            tokio::task::yield_now().await;

            // Run the agentic loop: call LLM, execute tools, repeat.
            run_agent_loop(
                ctx,
                &mgr,
                &cost_tracker,
                &budget_mgr,
                conv_id,
                agent_id,
                &model,
                &role,
                &history,
            )
            .await;
        }

        if !mgr.has_unprocessed(conv_id) {
            break;
        }
        debug!(
            conv_id,
            "new messages arrived during processing, continuing"
        );
    }

    let _ = mgr.set_processing_status(conv_id, "idle");
    ctx.event_bus.send(conv_id, ConversationEvent::Done);
    info!(conv_id, "background processing complete");
}

/// Agentic loop: stream from the LLM, execute tool calls, repeat until done.
#[allow(clippy::too_many_arguments)]
async fn run_agent_loop(
    ctx: &ProcessorContext,
    mgr: &ConversationManager,
    cost_tracker: &CostTracker,
    budget_mgr: &BudgetManager,
    conv_id: &str,
    agent_id: &str,
    model: &str,
    role: &str,
    history: &[crate::conversations::ConversationMessage],
) {
    use crate::conversations::tools;
    use crate::llm::router::{ToolCall, ToolCallFunction};
    use std::collections::HashMap;

    let system_prompt = format!(
        "{role}\n\n\
         When using tools, write ONE short sentence stating what you're about to \
         do before each tool call. Example: \"Searching memory for user context.\" \
         Keep it concise — no plans, no lists."
    );
    let mut llm_messages = vec![ChatMessage::text("system", &system_prompt)];
    for m in history {
        match m.sender_type.as_str() {
            "agent" => llm_messages.push(ChatMessage::text("assistant", &m.content)),
            "system" => {
                let sys_content = format!("SYSTEM: {}", m.content);
                llm_messages.push(ChatMessage::text("user", &sys_content));
            }
            _ => llm_messages.push(ChatMessage::text("user", &m.content)),
        }
    }

    let tool_defs = tools::tool_definitions();
    let max_turns = 10;
    let mut all_content = String::new();
    let mut last_text = String::new();
    let mut total_tokens: i64 = 0;
    let mut seen_calls: HashMap<String, String> = HashMap::new();
    let mut seen_reasoning: std::collections::HashSet<String> = std::collections::HashSet::new();

    for turn in 0..max_turns {
        let llm_req = ChatCompletionRequest {
            model: model.to_string(),
            messages: llm_messages.clone(),
            temperature: Some(0.7),
            max_tokens: Some(32768),
            stream: Some(true),
            tools: if tool_defs.is_empty() { None } else { Some(tool_defs.clone()) },
            ..Default::default()
        };

        // Stream the response, collecting text and tool call deltas
        let mut turn_text = String::new();
        let mut turn_reasoning = String::new();
        let mut pending_text = String::new();
        let mut in_thinking = false;
        let mut tool_call_acc: HashMap<i64, (String, String, String)> = HashMap::new();

        match ctx.llm_router.chat_stream(&llm_req).await {
            Ok(mut stream) => {
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(chunk) => {
                            if let Some(choice) = chunk.choices.first() {
                                // Stream reasoning content as <think> blocks
                                if let Some(ref reasoning) = choice.delta.reasoning_content {
                                    if !reasoning.is_empty() {
                                        turn_reasoning.push_str(reasoning);
                                        if !in_thinking {
                                            in_thinking = true;
                                            ctx.event_bus.send(
                                                conv_id,
                                                ConversationEvent::Chunk {
                                                    agent_id: agent_id.to_string(),
                                                    content: "<think>".to_string(),
                                                },
                                            );
                                        }
                                        ctx.event_bus.send(
                                            conv_id,
                                            ConversationEvent::Chunk {
                                                agent_id: agent_id.to_string(),
                                                content: reasoning.clone(),
                                            },
                                        );
                                    }
                                }
                                // Collect text content — we'll emit it after
                                // we know whether this turn has tool calls.
                                // Filter out model-specific garbage tokens.
                                if let Some(ref text) = choice.delta.content {
                                    let is_garbage = text.contains("<channel|>")
                                        || text.contains("<turn|>")
                                        || text.starts_with("thought")
                                        || text.contains("<|im_end|>")
                                        || text.contains("<|endoftext|>");
                                    if !text.is_empty() && !is_garbage {
                                        if in_thinking {
                                            in_thinking = false;
                                            ctx.event_bus.send(
                                                conv_id,
                                                ConversationEvent::Chunk {
                                                    agent_id: agent_id.to_string(),
                                                    content: "</think>".to_string(),
                                                },
                                            );
                                        }
                                        turn_text.push_str(text);
                                        total_tokens += 1;
                                        // Buffer text — emit after stream ends
                                        // so we can wrap it in <think> if tools follow
                                        pending_text.push_str(text);
                                    }
                                }
                                // Accumulate tool call deltas
                                if let Some(ref tcs) = choice.delta.tool_calls {
                                    for tc in tcs {
                                        let entry = tool_call_acc
                                            .entry(tc.index)
                                            .or_insert_with(|| (String::new(), String::new(), String::new()));
                                        if let Some(ref id) = tc.id {
                                            entry.0 = id.clone();
                                        }
                                        if let Some(ref func) = tc.function {
                                            if let Some(ref name) = func.name {
                                                entry.1 = name.clone();
                                            }
                                            if let Some(ref args) = func.arguments {
                                                entry.2.push_str(args);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(agent_id, error = %e, "stream error");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                warn!(conv_id, agent_id, error = %e, "LLM stream failed");
                ctx.event_bus.send(
                    conv_id,
                    ConversationEvent::Error {
                        agent_id: Some(agent_id.to_string()),
                        error: e.to_string(),
                    },
                );
                return;
            }
        }

        // Close any open thinking block from reasoning_content
        if in_thinking {
            ctx.event_bus.send(
                conv_id,
                ConversationEvent::Chunk {
                    agent_id: agent_id.to_string(),
                    content: "</think>".to_string(),
                },
            );
        }

        if !turn_text.is_empty() {
            last_text = turn_text.clone();
            all_content.push_str(&turn_text);
        }

        // No tool calls — emit buffered text as regular content and finish
        if tool_call_acc.is_empty() {
            if !pending_text.is_empty() {
                ctx.event_bus.send(
                    conv_id,
                    ConversationEvent::Chunk {
                        agent_id: agent_id.to_string(),
                        content: pending_text,
                    },
                );
            }
            break;
        }

        // Has tool calls — emit the reasoning text as a <think> block
        if !pending_text.is_empty() {
            let reasoning_chunk = format!("<think>{}</think>", pending_text.trim());
            ctx.event_bus.send(
                conv_id,
                ConversationEvent::Chunk {
                    agent_id: agent_id.to_string(),
                    content: reasoning_chunk,
                },
            );
        }

        let tool_calls: Vec<ToolCall> = {
            let mut tcs: Vec<ToolCall> = tool_call_acc
                .into_iter()
                .map(|(_, (id, name, args))| ToolCall {
                    id,
                    call_type: "function".into(),
                    function: ToolCallFunction { name, arguments: args },
                })
                .collect();
            tcs.sort_by_key(|tc| tc.id.clone());
            tcs
        };

        // --- Loop detection ---
        // Build a key from the tool calls (name + args). If the exact same
        // set of tool calls appears twice, the model is looping.
        let mut tool_key_parts: Vec<String> = tool_calls
            .iter()
            .map(|tc| format!("{}:{}", tc.function.name, tc.function.arguments))
            .collect();
        tool_key_parts.sort();
        let loop_key = tool_key_parts.join("|");

        if !loop_key.is_empty() && !seen_reasoning.insert(loop_key) {
            warn!(conv_id, agent_id, turn, "duplicate tool call set, breaking loop");
            break;
        }

        // Add assistant message with reasoning + tool calls
        llm_messages.push(ChatMessage {
            role: "assistant".into(),
            content: turn_text,
            tool_calls: Some(tool_calls.clone()),
            ..Default::default()
        });

        // Execute each tool call
        for tc in &tool_calls {
            let call_key = format!("{}:{}", tc.function.name, tc.function.arguments);

            let result = if let Some(prev) = seen_calls.get(&call_key) {
                warn!(conv_id, agent_id, tool = tc.function.name, "duplicate tool call blocked");
                format!(
                    "ALREADY DONE: You already called {} with these arguments. Result: {}\n\
                     Move on to the next step.",
                    tc.function.name, prev
                )
            } else {
                debug!(conv_id, agent_id, tool = tc.function.name, turn, "executing tool");
                let (r, _is_error) = tools::execute(
                    &tc.function.name,
                    &tc.function.arguments,
                    agent_id,
                    conv_id,
                    &ctx.db,
                )
                .await;
                seen_calls.insert(call_key, r.clone());
                r
            };

            // The reasoning was already streamed to the frontend above.
            // Now show the tool call.
            ctx.event_bus.send(
                conv_id,
                ConversationEvent::Chunk {
                    agent_id: agent_id.to_string(),
                    content: format!(
                        "\n<tool_call name=\"{}\">{}</tool_call>",
                        tc.function.name, tc.function.arguments
                    ),
                },
            );

            llm_messages.push(ChatMessage::tool_result(&tc.id, result));
        }
    }

    // If we exhausted max_turns, tell the model to create a continuation task
    if all_content.is_empty() && last_text.is_empty() {
        // The model never produced text — only tool calls.
        // Store a summary of what happened.
        last_text = format!(
            "I was working on your request and used several tools, but ran out of \
             turns before completing. I'll create a task to continue this work."
        );
        all_content = last_text.clone();
    }

    // Clean model-specific garbage from stored content
    fn clean_garbage(s: &str) -> String {
        s.replace("<channel|>", "")
            .replace("<turn|>", "")
            .replace("<|im_end|>", "")
            .replace("<|endoftext|>", "")
            .replace("thought\n", "")
            .trim()
            .to_string()
    }
    let cleaned_last = clean_garbage(&last_text);
    let cleaned_all = clean_garbage(&all_content);

    let store_content = if !cleaned_last.is_empty() {
        cleaned_last
    } else if !cleaned_all.is_empty() {
        cleaned_all
    } else {
        "(No response)".to_string()
    };
    record_and_store(
        mgr,
        cost_tracker,
        budget_mgr,
        &ctx.rate_limiter,
        ctx,
        conv_id,
        agent_id,
        model,
        &store_content,
        total_tokens,
    );

    // Inject task cards AFTER the agent's response, and broadcast them
    // so the frontend sees them live without needing a page reload.
    for (_, result) in &seen_calls {
        if let Some(rest) = result.strip_prefix("TASK_CREATED:") {
            let parts: Vec<&str> = rest.splitn(3, ':').collect();
            if parts.len() >= 2 {
                let task_id = parts[0];
                let title = parts[1];
                let card_json = serde_json::json!({
                    "task_id": task_id,
                    "title": title,
                    "status": "pending",
                    "subtasks_total": 0,
                    "subtasks_completed": 0,
                });
                if let Ok(msg) = mgr.send_message(
                    conv_id,
                    &SendMessage {
                        sender_type: "system".into(),
                        sender_id: agent_id.to_string(),
                        sender_name: Some(agent_id.to_string()),
                        content: card_json.to_string(),
                        message_type: Some("task_status".into()),
                    },
                ) {
                    ctx.event_bus.send(
                        conv_id,
                        ConversationEvent::Message {
                            message: serde_json::json!(msg),
                        },
                    );
                }
            }
        }
    }
}

/// Record costs, update budget, and store the agent message.
#[allow(clippy::too_many_arguments)]
fn record_and_store(
    mgr: &ConversationManager,
    cost_tracker: &CostTracker,
    budget_mgr: &BudgetManager,
    rate_limiter: &RateLimiter,
    ctx: &ProcessorContext,
    conv_id: &str,
    agent_id: &str,
    model: &str,
    content: &str,
    total_tokens: i64,
) {
    let input_tokens = total_tokens / 4;
    let output_tokens = total_tokens / 4;
    let _ = cost_tracker.record(
        agent_id,
        model,
        input_tokens,
        output_tokens,
        "chat",
        Some(conv_id),
    );
    let cost = cost_tracker
        .pricing()
        .calculate(model, input_tokens, output_tokens, 0, 0);
    let _ = budget_mgr.update_spending(agent_id, cost);
    rate_limiter.record_request(agent_id, (input_tokens + output_tokens) as u32);

    if let Ok(agent_msg) = mgr.send_message(
        conv_id,
        &SendMessage {
            sender_type: "agent".into(),
            sender_id: agent_id.to_string(),
            sender_name: Some(agent_id.to_string()),
            content: content.to_string(),
            message_type: None,
        },
    ) {
        ctx.event_bus.send(
            conv_id,
            ConversationEvent::Message {
                message: json!(agent_msg),
            },
        );

        // Route response back through connector if this is a channel-bound conversation
        route_response_to_channel(&ctx.db, conv_id, agent_id, content);
    }

    // Log activity for dashboard
    let activity = ActivityManager::new(ctx.db.clone());
    let _ = activity.log(
        "agent_response",
        Some(agent_id),
        Some(&json!({
            "conversation_id": conv_id,
            "model": model,
            "tokens": total_tokens,
            "response_len": content.len(),
        })),
        None,
    );
}

/// If this conversation is bound to a connector channel, send the agent's
/// response back through the connector.
fn route_response_to_channel(db: &Arc<Database>, conv_id: &str, _agent_id: &str, content: &str) {
    // Look up channel binding for this conversation
    let binding: Option<(String, String)> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT b.channel_id, c.connector_id
             FROM conversation_channel_bindings b
             JOIN connector_channels c ON c.id = b.channel_id
             WHERE b.conversation_id = ?1
             LIMIT 1",
            rusqlite::params![conv_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()
    });

    let Some((channel_id, connector_id)) = binding else {
        return; // Not a channel-bound conversation
    };

    let mgr = crate::connectors::manager::ConnectorManager::new(db.clone());
    let connector = match mgr.get(&connector_id) {
        Ok(c) => c,
        Err(_) => return,
    };
    let channel = match mgr.get_channel(&channel_id) {
        Ok(ch) => ch,
        Err(_) => return,
    };

    debug!(
        conv_id,
        connector = connector.name.as_str(),
        channel = channel.name.as_str(),
        "routing agent response back to connector channel"
    );

    crate::connectors::deliver::deliver(db, &connector.name, &channel.name, content);
}
