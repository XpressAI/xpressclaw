//! Background conversation processor (ADR-019).
//!
//! Processes agent responses in a background task. The conversation
//! continues regardless of whether a client is connected. Events are
//! broadcast to any subscribers via the event bus.

use std::sync::Arc;

use serde_json::json;
use tracing::{debug, info, warn};

use crate::agents::registry::AgentRegistry;
use crate::budget::manager::BudgetManager;
use crate::budget::rate_limiter::{RateLimitResult, RateLimiter};
use crate::budget::tracker::CostTracker;
use crate::config::Config;
use crate::conversations::event_bus::{ConversationEvent, ConversationEventBus};
use crate::conversations::{ConversationManager, SendMessage};
use crate::db::Database;
use crate::llm::router::{ChatCompletionRequest, ChatMessage, LlmRouter};

use futures_util::StreamExt;

/// Context needed by the processor. Built by the caller and passed in.
pub struct ProcessorContext {
    pub db: Arc<Database>,
    pub config: Arc<Config>,
    pub llm_router: Arc<LlmRouter>,
    pub event_bus: Arc<ConversationEventBus>,
    pub rate_limiter: Arc<RateLimiter>,
    /// Pre-computed agent roles (agent_id → system prompt with skills injected).
    /// If not provided, uses the raw role from config.
    pub agent_roles: std::collections::HashMap<String, String>,
    /// Shared Docker connection for harness tool execution.
    pub docker: Option<Arc<crate::docker::manager::DockerManager>>,
}

/// Spawn a background task to process agent responses for a conversation.
/// Returns immediately — the processing happens asynchronously.
pub fn spawn(conv_id: String, ctx: ProcessorContext) {
    tokio::spawn(async move {
        process_loop(&conv_id, &ctx).await;
    });
}

/// Process all pending user messages in a conversation.
/// Loops until there are no more unprocessed messages.
async fn process_loop(conv_id: &str, ctx: &ProcessorContext) {
    let mgr = ConversationManager::new(ctx.db.clone());

    // Mark conversation as processing
    let _ = mgr.set_processing_status(conv_id, "processing");

    loop {
        if !mgr.has_unprocessed(conv_id) {
            break;
        }

        // Resolve target agents
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

        // Mark messages as processed BEFORE generating response
        // so new messages arriving during generation start a new cycle
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

            // Verify agent exists in the registry
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

            // Use pre-computed role (with skills) if available, else raw config role
            let mut role = ctx.agent_roles.get(agent_id).cloned().unwrap_or_else(|| {
                agent_cfg
                    .map(|c| c.role.clone())
                    .unwrap_or_else(|| "You are a helpful AI assistant.".to_string())
            });

            // Inject tool descriptions so the model knows what's available.
            // Conversations stream directly from the LLM router (bypassing
            // the harness for streaming), so the model needs tool info in the
            // prompt. When it outputs <tool_call> tags, the harness executes them.
            let agent_tools = agent_cfg.map(|c| &c.tools).cloned().unwrap_or_default();
            if !agent_tools.is_empty() {
                let tool_desc = build_tool_descriptions(&agent_tools);
                role.push_str(&tool_desc);
            }

            let history = mgr.get_messages(conv_id, 20, None).unwrap_or_default();

            let mut llm_messages = vec![ChatMessage::text("system", &role)];
            for m in &history {
                match m.sender_type.as_str() {
                    "agent" => {
                        llm_messages.push(ChatMessage::text("assistant", &m.content));
                    }
                    "system" => {
                        // System messages are presented as user messages with
                        // a SYSTEM prefix so the model can see and react to them
                        // (e.g. task completion notifications).
                        let sys_content = format!("SYSTEM: {}", m.content);
                        llm_messages.push(ChatMessage::text("user", &sys_content));
                    }
                    _ => {
                        llm_messages.push(ChatMessage::text("user", &m.content));
                    }
                }
            }

            let llm_req = ChatCompletionRequest {
                model: model.clone(),
                messages: llm_messages,
                temperature: Some(0.7),
                max_tokens: Some(4096),
                stream: Some(true),
                ..Default::default()
            };

            // Broadcast thinking and yield so the SSE event reaches clients
            // before the LLM call (which may block for seconds during context init)
            ctx.event_bus.send(
                conv_id,
                ConversationEvent::Thinking {
                    agent_id: agent_id.clone(),
                },
            );
            tokio::task::yield_now().await;

            // Stream from LLM router for instant token-by-token display.
            // If the response contains tool calls (<tool_call> tags), we
            // then execute them via the harness and send a follow-up message.
            let stream_result = ctx.llm_router.chat_stream(&llm_req).await;

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
                                        total_tokens += text.len() as i64;
                                        ctx.event_bus.send(
                                            conv_id,
                                            ConversationEvent::Chunk {
                                                agent_id: agent_id.clone(),
                                                content: text.clone(),
                                            },
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                ctx.event_bus.send(
                                    conv_id,
                                    ConversationEvent::Error {
                                        agent_id: Some(agent_id.clone()),
                                        error: e.to_string(),
                                    },
                                );
                                break;
                            }
                        }
                    }

                    // Store the complete agent message
                    if !full_content.is_empty() {
                        let input_tokens = (llm_req
                            .messages
                            .iter()
                            .map(|m| m.content.len())
                            .sum::<usize>()
                            / 4) as i64;
                        let output_tokens = total_tokens / 4;
                        let _ = cost_tracker.record(
                            agent_id,
                            &model,
                            input_tokens,
                            output_tokens,
                            "chat",
                            Some(conv_id),
                        );
                        let cost = cost_tracker.pricing().calculate(
                            &model,
                            input_tokens,
                            output_tokens,
                            0,
                            0,
                        );
                        let _ = budget_mgr.update_spending(agent_id, cost);
                        ctx.rate_limiter
                            .record_request(agent_id, (input_tokens + output_tokens) as u32);

                        let has_tool_calls = full_content.contains("<tool_call");

                        if let Ok(agent_msg) = mgr.send_message(
                            conv_id,
                            &SendMessage {
                                sender_type: "agent".into(),
                                sender_id: agent_id.clone(),
                                sender_name: Some(agent_id.clone()),
                                content: full_content.clone(),
                                message_type: None,
                            },
                        ) {
                            ctx.event_bus.send(
                                conv_id,
                                ConversationEvent::Message {
                                    message: json!(agent_msg),
                                },
                            );
                        }

                        // If the LLM output contains tool calls, send the
                        // full conversation through the harness for execution.
                        // The harness runs the tool loop and returns the final
                        // result, which we send as a follow-up message.
                        if has_tool_calls {
                            if let Some(ref docker) = ctx.docker {
                                if let Some(ref cid) =
                                    registry.get(agent_id).ok().and_then(|a| a.container_id)
                                {
                                    if let Some(port) = docker.get_container_port(cid).await {
                                        let harness =
                                            crate::agents::harness::HarnessClient::new(port);
                                        // Build full conversation including the
                                        // tool-containing response
                                        let mut harness_msgs = llm_req.messages.clone();
                                        harness_msgs.push(crate::llm::router::ChatMessage::text(
                                            "assistant",
                                            &full_content,
                                        ));
                                        harness_msgs.push(crate::llm::router::ChatMessage::text(
                                            "user",
                                            "Execute the tool calls above and report the results.",
                                        ));
                                        let mut harness_req = llm_req.clone();
                                        harness_req.messages = harness_msgs;
                                        harness_req.stream = Some(false);

                                        if let Ok(resp) = harness.chat(&harness_req).await {
                                            let tool_result = resp
                                                .choices
                                                .first()
                                                .map(|c| c.message.content.clone())
                                                .unwrap_or_default();
                                            if !tool_result.is_empty() {
                                                if let Ok(tool_msg) = mgr.send_message(
                                                    conv_id,
                                                    &SendMessage {
                                                        sender_type: "agent".into(),
                                                        sender_id: agent_id.clone(),
                                                        sender_name: Some(agent_id.clone()),
                                                        content: tool_result,
                                                        message_type: None,
                                                    },
                                                ) {
                                                    ctx.event_bus.send(
                                                        conv_id,
                                                        ConversationEvent::Message {
                                                            message: json!(tool_msg),
                                                        },
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(conv_id, agent_id, error = %e, "LLM stream failed");
                    ctx.event_bus.send(
                        conv_id,
                        ConversationEvent::Error {
                            agent_id: Some(agent_id.clone()),
                            error: e.to_string(),
                        },
                    );
                }
            }
        }

        // Check if more messages arrived during processing
        if !mgr.has_unprocessed(conv_id) {
            break;
        }
        debug!(
            conv_id,
            "new messages arrived during processing, continuing"
        );
    }

    // Mark conversation as idle
    let _ = mgr.set_processing_status(conv_id, "idle");

    ctx.event_bus.send(conv_id, ConversationEvent::Done);

    info!(conv_id, "background processing complete");
}

/// Build a tool description block to append to the system prompt.
/// This tells the model what tools it has so it can decide to use them.
/// When the model outputs <tool_call> tags, the harness executes them.
fn build_tool_descriptions(tools: &[String]) -> String {
    let mut desc = String::from("\n\n## Available Tools\nYou have the following tools available. To use a tool, output a <tool_call> tag.\n\n");
    for tool in tools {
        match tool.as_str() {
            "filesystem" => desc
                .push_str("- **filesystem**: Read, write, edit, search files in the workspace\n"),
            "shell" => desc.push_str("- **shell**: Run shell commands\n"),
            "memory" => desc.push_str(
                "- **memory**: Save and search memories (search_memory, create_memory)\n",
            ),
            "fetch" => desc.push_str("- **fetch**: Fetch web pages and APIs (WebFetch)\n"),
            "websearch" => desc
                .push_str("- **web_search**: Search the web for current information (WebSearch)\n"),
            "git" => desc.push_str("- **git**: Interact with git repositories\n"),
            "github" => desc.push_str("- **github**: Manage GitHub issues, PRs, repos\n"),
            other => desc.push_str(&format!("- **{other}**\n")),
        }
    }
    desc.push_str(
        "\nUse <tool_call name=\"tool_name\">{\"arg\": \"value\"}</tool_call> to invoke a tool.\n",
    );
    desc
}
