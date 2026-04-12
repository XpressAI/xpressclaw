//! Task dispatcher — drives agents to complete tasks via a state machine.
//!
//! The dispatcher runs as a background polling loop that picks up queued tasks
//! from the TaskQueue, then runs each through a multi-turn state machine:
//!
//! ```text
//! LoadTask → BuildPrompt → CallAgent → ProcessResponse → Evaluate
//!                ↑                                           │
//!                └───────────── (more turns) ─────────────────┘
//! ```
//!
//! Each turn sends the task prompt to the agent's harness container and
//! processes the response. Subtasks are auto-advanced after each turn.

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, error, info, warn};

use serde_json::json;

use crate::activity::ActivityManager;
use crate::agents::registry::AgentRegistry;
use crate::config::Config;
use crate::config::HooksConfig;
use crate::conversations::{ConversationManager, SendMessage};
use crate::db::Database;
use crate::memory::hooks::{self, MemoryHooks};
use crate::tasks::board::{Task, TaskBoard, TaskStatus};
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::TaskQueue;
use crate::workflows::engine::WorkflowEngine;

const DEFAULT_MAX_TURNS: usize = 15;
const POLL_INTERVAL_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// State machine types
// ---------------------------------------------------------------------------

enum State {
    LoadTask,
    BuildPrompt,
    CallAgent,
    ProcessResponse,
    Evaluate,
    Done(DriverResult),
}

/// Terminal result of a task execution run.
#[derive(Debug)]
pub enum DriverResult {
    /// Task completed successfully.
    Completed,
    /// Task failed after exhausting turns or hitting an error.
    Failed(String),
    /// Task skipped (not found, wrong status, etc.).
    Skipped,
    /// Agent not ready — put back in queue for later.
    Requeue,
    /// Task paused — agent requested user input.
    Paused,
}

/// Mutable context carried between state machine transitions.
struct Context {
    task: Task,
    agent_id: String,
    system_prompt: String,
    turn: usize,
    max_turns: usize,
    current_prompt: String,
    last_response: String,
    subtasks: Vec<Task>,
    hooks: HooksConfig,
}

// ---------------------------------------------------------------------------
// State machine execution
// ---------------------------------------------------------------------------

async fn run_task(
    db: &Arc<Database>,
    config: &Config,
    task_id: &str,
    agent_id: &str,
) -> DriverResult {
    let mut state = State::LoadTask;
    let mut ctx: Option<Context> = None;

    loop {
        state = match state {
            State::LoadTask => load_task(db, config, task_id, agent_id, &mut ctx).await,
            State::BuildPrompt => build_prompt(db, ctx.as_mut().unwrap()),
            State::CallAgent => call_agent(db, config, ctx.as_mut().unwrap()).await,
            State::ProcessResponse => process_response(db, config, ctx.as_mut().unwrap()),
            State::Evaluate => evaluate(db, config, ctx.as_mut().unwrap()),
            State::Done(result) => return result,
        };
    }
}

async fn load_task(
    db: &Arc<Database>,
    config: &Config,
    task_id: &str,
    agent_id: &str,
    ctx: &mut Option<Context>,
) -> State {
    let board = TaskBoard::new(db.clone());

    // Fetch the task
    let task = match board.get(task_id) {
        Ok(t) => t,
        Err(e) => {
            warn!(task_id, error = %e, "task not found, skipping");
            return State::Done(DriverResult::Skipped);
        }
    };

    // Verify task is actionable
    match task.status {
        TaskStatus::Pending | TaskStatus::InProgress | TaskStatus::WaitingForInput => {}
        _ => {
            debug!(task_id, status = ?task.status, "task not actionable, skipping");
            return State::Done(DriverResult::Skipped);
        }
    }

    // Mark as in_progress if pending or resuming from waiting_for_input
    if task.status == TaskStatus::Pending || task.status == TaskStatus::WaitingForInput {
        if let Err(e) = board.update_status(task_id, "in_progress", Some(agent_id)) {
            warn!(task_id, error = %e, "failed to set task in_progress");
            return State::Done(DriverResult::Failed(e.to_string()));
        }
    }

    // Get agent config
    let agent_cfg = config.agents.iter().find(|a| a.name == agent_id);
    let system_prompt = agent_cfg
        .map(|a| a.full_system_prompt())
        .unwrap_or_else(|| "You are a helpful AI assistant.".to_string());

    // Verify agent is running and get harness port
    let registry = AgentRegistry::new(db.clone());
    let record = match registry.get(agent_id) {
        Ok(r) => r,
        Err(e) => {
            warn!(agent_id, error = %e, "agent not found, requeuing");
            return State::Done(DriverResult::Requeue);
        }
    };

    if record.status != "running" {
        debug!(
            agent_id,
            status = record.status,
            "agent not running, requeuing"
        );
        return State::Done(DriverResult::Requeue);
    }

    // Load subtasks
    let subtasks = board.list_subtasks(task_id).unwrap_or_default();
    let max_turns = if subtasks.is_empty() {
        DEFAULT_MAX_TURNS
    } else {
        DEFAULT_MAX_TURNS.max(subtasks.len() * 5)
    };

    info!(
        task_id,
        agent_id,
        subtasks = subtasks.len(),
        max_turns,
        "loaded task for execution"
    );

    let agent_hooks = agent_cfg.map(|a| a.hooks.clone()).unwrap_or_default();

    *ctx = Some(Context {
        task: board.get(task_id).unwrap_or(task),
        agent_id: agent_id.to_string(),
        system_prompt,
        turn: 0,
        max_turns,
        current_prompt: String::new(),
        last_response: String::new(),
        subtasks,
        hooks: agent_hooks,
    });

    State::BuildPrompt
}

fn build_prompt(db: &Arc<Database>, ctx: &mut Context) -> State {
    let conv = TaskConversation::new(db.clone());
    let history = conv.get_messages(&ctx.task.id).unwrap_or_default();

    let mut prompt = String::new();

    if ctx.turn == 0 {
        // Initial prompt — include the task ID so the agent can call complete_task
        prompt.push_str(&format!(
            "# Task: {}\nTask ID: {}\n\n",
            ctx.task.title, ctx.task.id
        ));
        if let Some(ref desc) = ctx.task.description {
            prompt.push_str(desc);
            prompt.push_str("\n\n");
        }

        // Include current subtask if any
        if let Some(subtask) = current_subtask(&ctx.subtasks) {
            prompt.push_str(&format!("## Current Step: {}\n", subtask.title));
            if let Some(ref desc) = subtask.description {
                prompt.push_str(desc);
                prompt.push_str("\n\n");
            }
        }

        if !ctx.subtasks.is_empty() {
            prompt.push_str("## All Steps:\n");
            for (i, st) in ctx.subtasks.iter().enumerate() {
                let marker = match st.status {
                    TaskStatus::Completed => "[x]",
                    TaskStatus::InProgress => "[>]",
                    _ => "[ ]",
                };
                prompt.push_str(&format!("{}. {} {}\n", i + 1, marker, st.title));
            }
            prompt.push('\n');
        }

        // Include completed dependency context (ADR-020).
        // Show the agent what its prerequisite tasks produced.
        let board = TaskBoard::new(db.clone());
        let deps = board.get_dependencies(&ctx.task.id).unwrap_or_default();
        if !deps.is_empty() {
            let mut dep_context = Vec::new();
            for dep_id in &deps {
                if let Ok(dep_task) = board.get(dep_id) {
                    let last_msg = conv
                        .get_messages(dep_id)
                        .unwrap_or_default()
                        .last()
                        .map(|m| m.content.clone())
                        .unwrap_or_else(|| "(no output)".to_string());
                    dep_context.push(format!(
                        "- ✅ \"{}\" — {}\n",
                        dep_task.title,
                        if last_msg.len() > 200 {
                            format!("{}...", &last_msg[..200])
                        } else {
                            last_msg
                        }
                    ));
                }
            }
            if !dep_context.is_empty() {
                prompt.push_str("## Completed Prerequisites\n");
                prompt.push_str("These tasks completed before yours. Their results:\n");
                for line in &dep_context {
                    prompt.push_str(line);
                }
                prompt.push('\n');
            }
        }

        if !ctx.subtasks.is_empty() {
            prompt.push_str(
                "Work on the current step. Mark each step complete as you finish it. \
                 When all steps are done, use `complete_task` to mark the parent complete.",
            );
        } else {
            prompt.push_str(&format!(
                "Work on this task using the tools available to you. \
                 For complex tasks, consider creating subtasks with `create_task` \
                 (set parent_task_id to \"{task_id}\") to show progress. \
                 When done, call `complete_task` with task_id \"{task_id}\". \
                 If you need clarification from the user, use `request_input`.",
                task_id = ctx.task.id,
            ));
        }
    } else {
        // Continuation prompt — the agent session preserves context,
        // so we don't replay history (that causes it to render inside
        // the system message in the UI). Just nudge the agent forward.
        prompt.push_str(&format!(
            "Continue working on the current task.\n\
             Task: {}\nDescription: {}\n\
             DO NOT call list_tasks — you already know what to do. \
             Focus on completing the work using Write, MakeDir, and other tools.",
            ctx.task.title,
            ctx.task.description.as_deref().unwrap_or("(no description)"),
        ));

        if let Some(subtask) = current_subtask(&ctx.subtasks) {
            prompt.push_str(&format!("\n\nCurrent step: {}", subtask.title));
            if let Some(ref desc) = subtask.description {
                prompt.push_str(&format!("\n{}", desc));
            }
        }

        // If the user sent a message, highlight it
        if let Some(last) = history.last() {
            if last.role == "user" {
                prompt.push_str(&format!(
                    "\n\nThe user responded:\n> {}",
                    truncate(&last.content, 500)
                ));
            }
        }

        prompt.push_str(&format!(
            "\n\nIMPORTANT: Call `complete_task` with task_id \"{}\" when done. \
             The task stays open until you call the tool.",
            ctx.task.id,
        ));
    }

    // Save the user-side prompt as a task message
    let _ = conv.add_message(&ctx.task.id, "system", &prompt);

    ctx.current_prompt = prompt;
    State::CallAgent
}

async fn call_agent(db: &Arc<Database>, config: &Config, ctx: &mut Context) -> State {
    use crate::conversations::tools;
    use crate::llm::router::{
        ChatCompletionRequest, ChatMessage, LlmRouter, ToolCall, ToolCallFunction,
    };
    use futures_util::StreamExt;
    use std::collections::HashMap;

    debug!(
        task_id = ctx.task.id,
        turn = ctx.turn,
        agent_id = ctx.agent_id,
        "calling LLM for task"
    );

    let llm_router = std::sync::Arc::new(crate::llm::router::LlmRouter::build_from_config(&config.llm));

    // Build messages — same system prompt + tool intent instruction as processor
    let system_with_tools = format!(
        "{}\n\nWhen using tools, write ONE short sentence stating what you're about \
         to do before each tool call. Example: \"Writing calculator.py with basic operations.\"",
        ctx.system_prompt
    );
    let mut llm_messages = vec![
        ChatMessage::text("system", &system_with_tools),
        ChatMessage::text("user", &ctx.current_prompt),
    ];

    let tool_defs = tools::task_tool_definitions(&ctx.task.id);
    let conv = TaskConversation::new(db.clone());
    let streaming_msg = conv.add_message(&ctx.task.id, "assistant", "").ok();
    let msg_id = streaming_msg.map(|m| m.id);

    let mut full_content = String::new();
    let max_tool_turns = 10;
    let mut seen_tool_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_name_counts: HashMap<String, usize> = HashMap::new();

    for tool_turn in 0..max_tool_turns {
        let llm_req = ChatCompletionRequest {
            model: config
                .agents
                .iter()
                .find(|a| a.name == ctx.agent_id)
                .and_then(|a| a.model.clone())
                .unwrap_or_else(|| "default".to_string()),
            messages: llm_messages.clone(),
            temperature: Some(0.7),
            max_tokens: Some(32768),
            stream: Some(true),
            tools: Some(tool_defs.clone()),
            ..Default::default()
        };

        let mut turn_text = String::new();
        let mut tool_call_acc: HashMap<i64, (String, String, String)> = HashMap::new();

        match llm_router.chat_stream(&llm_req).await {
            Ok(mut stream) => {
                while let Some(result) = stream.next().await {
                    if let Ok(chunk) = result {
                        if let Some(choice) = chunk.choices.first() {
                            // Capture reasoning_content as text (many models
                            // put all output there instead of content)
                            if let Some(ref reasoning) = choice.delta.reasoning_content {
                                if !reasoning.is_empty() {
                                    // Don't add to turn_text — reasoning is internal
                                }
                            }
                            if let Some(ref text) = choice.delta.content {
                                // Filter model-specific garbage
                                let is_garbage = text.contains("<channel|>")
                                    || text.contains("<turn|>")
                                    || text.starts_with("thought")
                                    || text.contains("<|im_end|>")
                                    || text.contains("<|endoftext|>");
                                if !text.is_empty() && !is_garbage {
                                    turn_text.push_str(text);
                                }
                            }
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
                }
            }
            Err(e) => {
                return State::Done(DriverResult::Failed(format!("LLM error: {e}")));
            }
        }

        if !turn_text.is_empty() {
            full_content = turn_text.clone();
        }

        // Update streaming message
        if let Some(id) = msg_id {
            let display = if full_content.is_empty() { "(working...)" } else { &full_content };
            let _ = conv.update_message_content(id, display);
        }

        if tool_call_acc.is_empty() {
            break;
        }

        let tool_calls: Vec<ToolCall> = tool_call_acc
            .into_iter()
            .map(|(_, (id, name, args))| ToolCall {
                id,
                call_type: "function".into(),
                function: ToolCallFunction { name, arguments: args },
            })
            .collect();

        // Loop detection
        let mut tool_key_parts: Vec<String> = tool_calls
            .iter()
            .map(|tc| format!("{}:{}", tc.function.name, tc.function.arguments))
            .collect();
        tool_key_parts.sort();
        let loop_key = tool_key_parts.join("|");
        if !loop_key.is_empty() && !seen_tool_keys.insert(loop_key) {
            warn!(task_id = ctx.task.id, tool_turn, "duplicate tool call set, breaking");
            break;
        }
        // Also break if any single tool is called too many times
        for tc in &tool_calls {
            let count = tool_name_counts.entry(tc.function.name.clone()).or_insert(0);
            *count += 1;
            if *count > 3 {
                warn!(task_id = ctx.task.id, tool = tc.function.name, count, "tool called too many times, breaking");
                // Don't execute this turn's tools
                break;
            }
        }

        llm_messages.push(ChatMessage {
            role: "assistant".into(),
            content: turn_text,
            tool_calls: Some(tool_calls.clone()),
            ..Default::default()
        });

        for tc in &tool_calls {
            let (result, _) = tools::execute(
                &tc.function.name,
                &tc.function.arguments,
                &ctx.agent_id,
                ctx.task.conversation_id.as_deref().unwrap_or(&ctx.task.id),
                db,
            )
            .await;

            // Record what the agent did so the task message isn't empty
            let action = format!("**{}**: {}", tc.function.name, result.chars().take(200).collect::<String>());
            if full_content.is_empty() {
                full_content = action;
            } else {
                full_content.push_str("\n");
                full_content.push_str(&action);
            }

            // Update the streaming message with progress
            if let Some(id) = msg_id {
                let _ = conv.update_message_content(id, &full_content);
            }

            llm_messages.push(ChatMessage::tool_result(&tc.id, result));
        }
    }

    // Final update
    if let Some(id) = msg_id {
        let display = if full_content.is_empty() { "(No response)" } else { &full_content };
        let _ = conv.update_message_content(id, display);
    }

    ctx.last_response = full_content;
    State::ProcessResponse
}

fn process_response(db: &Arc<Database>, config: &Config, ctx: &mut Context) -> State {
    // Message is already saved incrementally by call_agent's streaming loop.
    ctx.turn += 1;

    // Memory hooks disabled — no harness port for sub-agent recall.
    // TODO: implement memory hooks via Wanix or direct LLM calls.

    debug!(
        task_id = ctx.task.id,
        turn = ctx.turn,
        response_len = ctx.last_response.len(),
        "processed agent response"
    );

    State::Evaluate
}

fn evaluate(db: &Arc<Database>, config: &Config, ctx: &mut Context) -> State {
    let board = TaskBoard::new(db.clone());

    // Reload task status (agent may have completed it via MCP tools)
    if let Ok(task) = board.get(&ctx.task.id) {
        ctx.task = task;
    }

    // If the agent already completed the task via tools, we're done
    if ctx.task.status == TaskStatus::Completed {
        info!(
            task_id = ctx.task.id,
            turns = ctx.turn,
            "task completed by agent"
        );
        return State::Done(DriverResult::Completed);
    }

    if ctx.task.status == TaskStatus::Cancelled {
        info!(task_id = ctx.task.id, "task was cancelled");
        return State::Done(DriverResult::Skipped);
    }

    if ctx.task.status == TaskStatus::WaitingForInput {
        info!(
            task_id = ctx.task.id,
            turns = ctx.turn,
            "task paused, waiting for user input"
        );
        return State::Done(DriverResult::Paused);
    }

    // IDLE tasks auto-complete after a single agent response and get deleted.
    // They are single-turn check-ins, not multi-turn conversations.
    if ctx.task.task_type == "IDLE" {
        info!(
            task_id = ctx.task.id,
            agent_id = ctx.agent_id,
            "idle task auto-completing after agent response"
        );
        let _ = board.delete(&ctx.task.id);
        return State::Done(DriverResult::Completed);
    }

    // Auto-advance subtasks
    if !ctx.subtasks.is_empty() {
        // Refresh subtask status
        ctx.subtasks = board.list_subtasks(&ctx.task.id).unwrap_or_default();

        // Find current in_progress subtask and advance it
        let in_progress_idx = ctx
            .subtasks
            .iter()
            .position(|s| s.status == TaskStatus::InProgress);

        if let Some(idx) = in_progress_idx {
            // Mark current as completed
            let _ = board.update_status(&ctx.subtasks[idx].id, "completed", None);

            // Find next pending and mark in_progress
            let next_pending = ctx
                .subtasks
                .iter()
                .skip(idx + 1)
                .find(|s| s.status == TaskStatus::Pending);
            if let Some(next) = next_pending {
                let _ = board.update_status(&next.id, "in_progress", Some(&ctx.agent_id));
            }
        } else {
            // No in_progress subtask — start the first pending one
            let first_pending = ctx
                .subtasks
                .iter()
                .find(|s| s.status == TaskStatus::Pending);
            if let Some(first) = first_pending {
                let _ = board.update_status(&first.id, "in_progress", Some(&ctx.agent_id));
            }
        }

        // Refresh again
        ctx.subtasks = board.list_subtasks(&ctx.task.id).unwrap_or_default();

        // Send progress update to conversation if subtasks advanced
        notify_conversation(db, config, &ctx.task.id, &ctx.agent_id, "in_progress");

        // Check if all subtasks are done
        let all_done = ctx
            .subtasks
            .iter()
            .all(|s| s.status == TaskStatus::Completed || s.status == TaskStatus::Cancelled);

        if all_done {
            let _ = board.update_status(&ctx.task.id, "completed", None);
            info!(
                task_id = ctx.task.id,
                turns = ctx.turn,
                "all subtasks completed"
            );
            return State::Done(DriverResult::Completed);
        }
    }

    // Check turn limit
    if ctx.turn >= ctx.max_turns {
        warn!(
            task_id = ctx.task.id,
            turns = ctx.turn,
            max = ctx.max_turns,
            "max turns exceeded"
        );
        return State::Done(DriverResult::Failed("max turns exceeded".to_string()));
    }

    // Continue to next turn
    debug!(
        task_id = ctx.task.id,
        turn = ctx.turn,
        max_turns = ctx.max_turns,
        "continuing to next turn"
    );
    State::BuildPrompt
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn current_subtask(subtasks: &[Task]) -> Option<&Task> {
    subtasks
        .iter()
        .find(|s| s.status == TaskStatus::InProgress)
        .or_else(|| subtasks.iter().find(|s| s.status == TaskStatus::Pending))
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

/// Reset an agent's idle_count to 0 after completing real work.
/// This ensures the next idle check fires promptly.
fn reset_idle_count(db: &Arc<Database>, agent_id: &str) {
    let now = chrono::Utc::now()
        .naive_utc()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    if let Err(e) = db.with_conn(|conn| {
        conn.execute(
            "UPDATE agents SET idle_count = 0, last_idle_check = ?1 WHERE id = ?2",
            rusqlite::params![now, agent_id],
        )
        .map_err(|e| crate::error::Error::Database(e.to_string()))
    }) {
        debug!(agent_id, error = %e, "failed to reset idle_count");
    }
}

/// Send a task status notification back to the originating conversation.
/// Includes subtask progress (completed/total) for inline progress bars.
fn notify_conversation(
    db: &Arc<Database>,
    config: &Config,
    task_id: &str,
    agent_id: &str,
    status: &str,
) {
    let board = TaskBoard::new(db.clone());
    let task = match board.get(task_id) {
        Ok(t) => t,
        Err(_) => return,
    };

    let conv_id = match task.conversation_id {
        Some(ref id) => id.clone(),
        None => return,
    };

    // Count subtask progress
    let subtasks = board.list_subtasks(task_id).unwrap_or_default();
    let total = subtasks.len();
    let completed = subtasks
        .iter()
        .filter(|s| s.status == TaskStatus::Completed)
        .count();

    let mgr = ConversationManager::new(db.clone());
    let content = json!({
        "task_id": task.id,
        "title": task.title,
        "status": status,
        "subtasks_total": total,
        "subtasks_completed": completed,
    })
    .to_string();

    // Use display_name if available, fall back to agent_id
    let display_name = config
        .agents
        .iter()
        .find(|a| a.name == agent_id)
        .and_then(|a| a.display_name.clone())
        .unwrap_or_else(|| agent_id.to_string());

    // Update the existing task_status message if one exists for this task,
    // otherwise create a new one. This keeps a single card in the conversation
    // that transitions through pending → in_progress → completed/failed.
    let updated = mgr.update_task_status_message(&conv_id, task_id, &content);
    if !updated {
        if let Err(e) = mgr.send_message(
            &conv_id,
            &SendMessage {
                sender_type: "system".into(),
                sender_id: agent_id.to_string(),
                sender_name: Some(display_name.clone()),
                content,
                message_type: Some("task_status".into()),
            },
        ) {
            warn!(task_id, conv_id, error = %e, "failed to notify conversation of task status");
        }
    }

    // On completion or failure, send an unprocessed "user" message so the
    // conversation processor wakes the agent to respond about the result.
    // Uses sender_type "user" because the processor only checks for
    // unprocessed user messages (system messages are user messages with
    // a SYSTEM prefix in this architecture).
    if status == "completed" || status == "failed" {
        let wake_content = format!(
            "SYSTEM: Background task \"{}\" has {}. Please acknowledge this to the user.",
            task.title, status
        );
        let _ = mgr.send_user_message(
            &conv_id,
            &SendMessage {
                sender_type: "user".into(),
                sender_id: "system".to_string(),
                sender_name: Some("System".to_string()),
                content: wake_content,
                message_type: Some("task_wake".into()),
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Polling loop
// ---------------------------------------------------------------------------

/// Start the task dispatcher background loop.
///
/// On startup, recovers interrupted tasks (queue items stuck in 'running'
/// state from a previous crash, and in_progress tasks with no queue entry).
/// Then polls every few seconds for queued items and runs them.
pub async fn start_dispatcher(db: Arc<Database>, config: Arc<Config>) {
    info!("task dispatcher started");

    if let Err(e) = recover_on_startup(&db) {
        error!(error = %e, "failed to recover tasks on startup");
    }

    loop {
        if let Err(e) = poll_once(&db, &config).await {
            error!(error = %e, "dispatcher poll error");
        }
        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

/// Recover tasks that were interrupted by a server restart.
fn recover_on_startup(db: &Arc<Database>) -> crate::error::Result<()> {
    let queue = TaskQueue::new(db.clone());
    let board = TaskBoard::new(db.clone());

    // 1. Reset 'running' queue items back to 'queued' — they were mid-execution
    let running = queue.list(None, Some("running"), 100)?;
    for item in &running {
        let _ = queue.fail(item.id, "interrupted by restart");
        let _ = queue.enqueue(&item.task_id, &item.agent_id);
        info!(
            task_id = item.task_id,
            agent_id = item.agent_id,
            "recovered interrupted queue item"
        );
    }

    // 2. Re-enqueue in_progress tasks that have no active queue entry
    let in_progress = board.list_all(Some("in_progress"), None, 100)?;
    for task in &in_progress {
        let agent_id = match &task.agent_id {
            Some(id) => id.clone(),
            None => continue,
        };

        // Check if there's already a queued/running entry for this task
        let existing = queue.list(None, Some("queued"), 100)?;
        let already_queued = existing.iter().any(|q| q.task_id == task.id);
        if already_queued {
            continue;
        }

        let _ = queue.enqueue(&task.id, &agent_id);
        info!(
            task_id = task.id,
            agent_id, "re-enqueued orphaned in_progress task"
        );
    }

    if !running.is_empty() || !in_progress.is_empty() {
        info!(
            running_recovered = running.len(),
            orphaned_recovered = in_progress.len(),
            "task recovery complete"
        );
    }

    Ok(())
}

async fn poll_once(db: &Arc<Database>, config: &Config) -> crate::error::Result<()> {
    let queue = TaskQueue::new(db.clone());
    let board = TaskBoard::new(db.clone());

    // Get all agents that have queued items
    let queued = queue.list(None, Some("queued"), 50)?;
    if queued.is_empty() {
        return Ok(());
    }

    // Deduplicate by agent (one task per agent per poll).
    // Only dispatch to agents that are actually running.
    let registry = AgentRegistry::new(db.clone());
    let mut seen_agents = std::collections::HashSet::new();
    for item in &queued {
        if !seen_agents.insert(item.agent_id.clone()) {
            continue;
        }

        // Skip agents that aren't running — tasks stay queued until the agent starts
        match registry.get(&item.agent_id) {
            Ok(record) if record.status == "running" => {}
            _ => continue,
        }

        // Claim the item atomically
        let claimed = match queue.claim(&item.agent_id)? {
            Some(c) => c,
            None => continue,
        };

        // Check dependencies before dispatching (ADR-020).
        // If not all dependencies are completed, skip this task for now.
        if !board.is_ready(&claimed.task_id).unwrap_or(true) {
            debug!(
                task_id = claimed.task_id,
                "task has unmet dependencies, re-queuing"
            );
            let _ = queue.fail(claimed.id, "dependencies not met");
            let _ = queue.enqueue(&claimed.task_id, &claimed.agent_id);
            continue;
        }

        info!(
            task_id = claimed.task_id,
            agent_id = claimed.agent_id,
            queue_id = claimed.id,
            "dispatching task"
        );

        let result = run_task(db, config, &claimed.task_id, &claimed.agent_id).await;

        match &result {
            DriverResult::Completed => {
                let _ = queue.complete(claimed.id, "completed");
                // IDLE tasks delete themselves in evaluate(); for normal tasks
                // mark completed and reset the agent's idle_count so the next
                // idle check fires promptly.
                let is_idle = board
                    .get(&claimed.task_id)
                    .map(|t| t.task_type == "IDLE")
                    .unwrap_or(false);
                if !is_idle {
                    let _ = board.update_status(&claimed.task_id, "completed", None);
                    reset_idle_count(db, &claimed.agent_id);
                    notify_conversation(
                        db,
                        config,
                        &claimed.task_id,
                        &claimed.agent_id,
                        "completed",
                    );
                }
                // Log activity
                if !is_idle {
                    let activity = ActivityManager::new(db.clone());
                    let title = board
                        .get(&claimed.task_id)
                        .map(|t| t.title)
                        .unwrap_or_default();
                    let _ = activity.log(
                        "task_completed",
                        Some(&claimed.agent_id),
                        Some(&json!({
                            "task_id": claimed.task_id,
                            "title": title,
                        })),
                        None,
                    );
                }
                info!(task_id = claimed.task_id, "task completed");
                // Advance workflow if this task is part of one
                advance_workflow(db, &claimed.task_id, "completed");
            }
            DriverResult::Failed(reason) => {
                let _ = queue.fail(claimed.id, reason);
                let _ = board.update_status(&claimed.task_id, "cancelled", None);
                warn!(task_id = claimed.task_id, reason, "task execution failed");
                notify_conversation(db, config, &claimed.task_id, &claimed.agent_id, "failed");
                // Advance workflow if this task is part of one
                advance_workflow(db, &claimed.task_id, "failed");
            }
            DriverResult::Skipped => {
                let _ = queue.complete(claimed.id, "skipped");
                debug!(task_id = claimed.task_id, "task skipped");
            }
            DriverResult::Requeue => {
                // Put back in queue — will be picked up on next poll
                let _ = queue.fail(claimed.id, "agent not ready");
                let _ = queue.enqueue(&claimed.task_id, &claimed.agent_id);
                debug!(
                    task_id = claimed.task_id,
                    "task requeued (agent became unavailable mid-execution)"
                );
            }
            DriverResult::Paused => {
                let _ = queue.complete(claimed.id, "waiting_for_input");
                info!(task_id = claimed.task_id, "task paused for user input");
            }
        }
    }

    Ok(())
}

/// If a completed/failed task is part of a workflow, advance the workflow.
fn advance_workflow(db: &Arc<Database>, task_id: &str, status: &str) {
    let engine = WorkflowEngine::new(db.clone());
    match engine.find_execution_by_task(task_id) {
        Ok(Some(_)) => {
            // Get the last assistant message as output
            let tc = TaskConversation::new(db.clone());
            let output = tc
                .get_messages(task_id)
                .unwrap_or_default()
                .into_iter()
                .rev()
                .find(|m| m.role == "assistant")
                .map(|m| m.content)
                .unwrap_or_default();

            if let Err(e) = engine.on_task_completed(task_id, status, &output) {
                warn!(task_id, error = %e, "failed to advance workflow");
            }
        }
        Ok(None) => {} // Not part of a workflow
        Err(e) => {
            debug!(task_id, error = %e, "workflow lookup failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::board::CreateTask;

    fn setup() -> Arc<Database> {
        Arc::new(Database::open_memory().unwrap())
    }

    #[test]
    fn test_current_subtask_finds_in_progress() {
        let subtasks = vec![
            Task {
                id: "1".into(),
                title: "Step 1".into(),
                status: TaskStatus::Completed,
                description: None,
                priority: 0,
                agent_id: None,
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                created_at: String::new(),
                updated_at: String::new(),
                completed_at: None,
                context: None,
                task_type: "normal".into(),
                hidden: false,
            },
            Task {
                id: "2".into(),
                title: "Step 2".into(),
                status: TaskStatus::InProgress,
                description: None,
                priority: 0,
                agent_id: None,
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                created_at: String::new(),
                updated_at: String::new(),
                completed_at: None,
                context: None,
                task_type: "normal".into(),
                hidden: false,
            },
            Task {
                id: "3".into(),
                title: "Step 3".into(),
                status: TaskStatus::Pending,
                description: None,
                priority: 0,
                agent_id: None,
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                created_at: String::new(),
                updated_at: String::new(),
                completed_at: None,
                context: None,
                task_type: "normal".into(),
                hidden: false,
            },
        ];

        let current = current_subtask(&subtasks).unwrap();
        assert_eq!(current.id, "2");
    }

    #[test]
    fn test_current_subtask_falls_back_to_pending() {
        let subtasks = vec![Task {
            id: "1".into(),
            title: "Step 1".into(),
            status: TaskStatus::Pending,
            description: None,
            priority: 0,
            agent_id: None,
            parent_task_id: None,
            sop_id: None,
            conversation_id: None,
            created_at: String::new(),
            updated_at: String::new(),
            completed_at: None,
            context: None,
            task_type: "normal".into(),
            hidden: false,
        }];

        let current = current_subtask(&subtasks).unwrap();
        assert_eq!(current.id, "1");
    }

    #[test]
    fn test_current_subtask_none_when_all_done() {
        let subtasks = vec![Task {
            id: "1".into(),
            title: "Step 1".into(),
            status: TaskStatus::Completed,
            description: None,
            priority: 0,
            agent_id: None,
            parent_task_id: None,
            sop_id: None,
            conversation_id: None,
            created_at: String::new(),
            updated_at: String::new(),
            completed_at: None,
            context: None,
            task_type: "normal".into(),
            hidden: false,
        }];

        assert!(current_subtask(&subtasks).is_none());
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn test_enqueue_and_skip_invalid_task() {
        let db = setup();
        let board = TaskBoard::new(db.clone());
        let queue = TaskQueue::new(db.clone());

        // Create a completed task — dispatcher should skip it
        let task = board
            .create(&CreateTask {
                title: "Already done".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        board.update_status(&task.id, "completed", None).unwrap();

        queue.enqueue(&task.id, "atlas").unwrap();

        // The task is completed, so load_task should skip it
        // (We can't run the full async state machine in a sync test,
        // but we verify the queue+board setup is correct)
        let task = board.get(&task.id).unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
    }
}
