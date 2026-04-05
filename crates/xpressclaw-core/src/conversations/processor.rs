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
    /// Shared Docker connection for harness access.
    pub docker: Option<Arc<crate::docker::manager::DockerManager>>,
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

            // Get the last user message to send to the session
            let history = mgr.get_messages(conv_id, 5, None).unwrap_or_default();
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

            if last_user_msg.is_empty() {
                continue;
            }

            // Memory recall hook: on first turn, if agent has memories,
            // spawn a sub-agent to synthesize a recollection.
            let harness_port = get_harness_port(ctx, &registry, agent_id).await;
            let agent_hooks = agent_cfg.map(|c| &c.hooks);
            let mut recollection: Option<String> = None;

            if let Some(port) = harness_port {
                if let Some(hooks_cfg) = agent_hooks {
                    if hooks::has_recall_hook(hooks_cfg) {
                        let eviction = &ctx.config.memory.eviction;
                        let mem_hooks = MemoryHooks::new(ctx.db.clone(), eviction);
                        recollection = mem_hooks.recall(agent_id, &last_user_msg, port).await;
                    }
                }
            }

            // If we got a recollection, inject it as a system message
            // (visible to the agent but doesn't change the system prompt).
            if let Some(ref recall_text) = recollection {
                let _ = mgr.send_message(
                    conv_id,
                    &SendMessage {
                        sender_type: "system".into(),
                        sender_id: "memory".to_string(),
                        sender_name: Some("Memory".to_string()),
                        content: format!("*Recollection:* {recall_text}"),
                        message_type: Some("memory_recall".into()),
                    },
                );
            }

            // Broadcast "thinking" event
            ctx.event_bus.send(
                conv_id,
                ConversationEvent::Thinking {
                    agent_id: agent_id.clone(),
                },
            );
            tokio::task::yield_now().await;

            if let Some(port) = harness_port {
                let harness = crate::agents::harness::HarnessClient::new(port);

                // Send to the agent's persistent session
                match harness
                    .send_session_message(&last_user_msg, conv_id, &sender_name, "user", &role)
                    .await
                {
                    Ok(mut stream) => {
                        let mut full_content = String::new();
                        let mut total_tokens: i64 = 0;

                        while let Some(result) = stream.next().await {
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
                                    warn!(agent_id, error = %e, "session stream error");
                                    break;
                                }
                            }
                        }

                        if !full_content.is_empty() {
                            record_and_store(
                                &mgr,
                                &cost_tracker,
                                &budget_mgr,
                                &ctx.rate_limiter,
                                ctx,
                                conv_id,
                                agent_id,
                                &model,
                                &full_content,
                                total_tokens,
                            );

                            // Async memory remember hook — runs in background
                            if let Some(hooks_cfg) = agent_hooks {
                                if hooks::has_remember_hook(hooks_cfg) {
                                    let db = ctx.db.clone();
                                    let eviction = ctx.config.memory.eviction.clone();
                                    let aid = agent_id.to_string();
                                    let user_msg = last_user_msg.clone();
                                    let resp = full_content.clone();
                                    let hp = port;
                                    tokio::spawn(async move {
                                        let mem_hooks = MemoryHooks::new(db, &eviction);
                                        mem_hooks.remember(&aid, &user_msg, &resp, hp).await;
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(agent_id, error = %e, "harness session failed, falling back to LLM router");
                        // Fall back to LLM router streaming
                        stream_from_llm_router(
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
                }
            } else {
                // No harness container — stream from LLM router (no tools)
                stream_from_llm_router(
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

/// Get the harness port for an agent's container.
async fn get_harness_port(
    ctx: &ProcessorContext,
    registry: &AgentRegistry,
    agent_id: &str,
) -> Option<u16> {
    let docker = ctx.docker.as_ref()?;
    let record = registry.get(agent_id).ok()?;
    let cid = record.container_id.as_ref()?;
    docker.get_container_port(cid).await
}

/// Fall back to streaming from the LLM router (no tools, but responsive).
#[allow(clippy::too_many_arguments)]
async fn stream_from_llm_router(
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
    let mut llm_messages = vec![ChatMessage::text("system", role)];
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

    let llm_req = ChatCompletionRequest {
        model: model.to_string(),
        messages: llm_messages,
        temperature: Some(0.7),
        max_tokens: Some(4096),
        stream: Some(true),
        ..Default::default()
    };

    match ctx.llm_router.chat_stream(&llm_req).await {
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
                                        agent_id: agent_id.to_string(),
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
                                agent_id: Some(agent_id.to_string()),
                                error: e.to_string(),
                            },
                        );
                        break;
                    }
                }
            }

            if !full_content.is_empty() {
                record_and_store(
                    mgr,
                    cost_tracker,
                    budget_mgr,
                    &ctx.rate_limiter,
                    ctx,
                    conv_id,
                    agent_id,
                    model,
                    &full_content,
                    total_tokens,
                );
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
    }
}
