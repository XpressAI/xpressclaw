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
    /// Shared Docker connection to avoid socket exhaustion.
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

            let agent = match registry.get(agent_id) {
                Ok(a) => a,
                Err(_) => continue,
            };
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
            let role = ctx.agent_roles.get(agent_id).cloned().unwrap_or_else(|| {
                agent_cfg
                    .map(|c| c.role.clone())
                    .unwrap_or_else(|| "You are a helpful AI assistant.".to_string())
            });

            let history = mgr.get_messages(conv_id, 20, None).unwrap_or_default();

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

            // Broadcast thinking and yield so the SSE event reaches clients
            // before the LLM call (which may block for seconds during context init)
            ctx.event_bus.send(
                conv_id,
                ConversationEvent::Thinking {
                    agent_id: agent_id.clone(),
                },
            );
            tokio::task::yield_now().await;

            // Route through harness or LLM router.
            // Uses the shared Docker connection to avoid socket exhaustion.
            let stream_result = if let Some(ref cid) = agent.container_id {
                let port = if let Some(ref docker) = ctx.docker {
                    docker.get_container_port(cid).await
                } else {
                    None
                };
                if let Some(port) = port {
                    let harness = crate::agents::harness::HarnessClient::new(port);
                    harness.chat_stream(&llm_req).await
                } else {
                    ctx.llm_router.chat_stream(&llm_req).await
                }
            } else {
                ctx.llm_router.chat_stream(&llm_req).await
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

                        if let Ok(agent_msg) = mgr.send_message(
                            conv_id,
                            &SendMessage {
                                sender_type: "agent".into(),
                                sender_id: agent_id.clone(),
                                sender_name: Some(agent_id.clone()),
                                content: full_content,
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
