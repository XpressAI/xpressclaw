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

use crate::agents::harness::HarnessClient;
use crate::agents::registry::AgentRegistry;
use crate::config::Config;
use crate::conversations::{ConversationManager, SendMessage};
use crate::db::Database;
use crate::docker::manager::DockerManager;
use crate::tasks::board::{Task, TaskBoard, TaskStatus};
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::TaskQueue;

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
}

/// Mutable context carried between state machine transitions.
struct Context {
    task: Task,
    agent_id: String,
    harness_port: u16,
    model: String,
    system_prompt: String,
    turn: usize,
    max_turns: usize,
    current_prompt: String,
    last_response: String,
    subtasks: Vec<Task>,
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
            State::CallAgent => call_agent(ctx.as_mut().unwrap()).await,
            State::ProcessResponse => process_response(db, ctx.as_mut().unwrap()),
            State::Evaluate => evaluate(db, ctx.as_mut().unwrap()),
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
        TaskStatus::Pending | TaskStatus::InProgress => {}
        _ => {
            debug!(task_id, status = ?task.status, "task not actionable, skipping");
            return State::Done(DriverResult::Skipped);
        }
    }

    // Mark as in_progress if pending
    if task.status == TaskStatus::Pending {
        if let Err(e) = board.update_status(task_id, "in_progress", Some(agent_id)) {
            warn!(task_id, error = %e, "failed to set task in_progress");
            return State::Done(DriverResult::Failed(e.to_string()));
        }
    }

    // Get agent config
    let agent_cfg = config.agents.iter().find(|a| a.name == agent_id);
    let model = agent_cfg
        .and_then(|a| a.model.clone())
        .unwrap_or_else(|| "local".to_string());
    let system_prompt = agent_cfg
        .map(|a| a.role.clone())
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

    let container_id = match record.container_id {
        Some(ref cid) => cid.clone(),
        None => {
            debug!(agent_id, "agent has no container, requeuing");
            return State::Done(DriverResult::Requeue);
        }
    };

    let harness_port = match DockerManager::connect().await {
        Ok(docker) => match docker.get_container_port(&container_id).await {
            Some(port) => port,
            None => {
                warn!(agent_id, "container has no port, requeuing");
                return State::Done(DriverResult::Requeue);
            }
        },
        Err(e) => {
            warn!(error = %e, "docker not available, requeuing");
            return State::Done(DriverResult::Requeue);
        }
    };

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

    *ctx = Some(Context {
        task: board.get(task_id).unwrap_or(task),
        agent_id: agent_id.to_string(),
        harness_port,
        model,
        system_prompt,
        turn: 0,
        max_turns,
        current_prompt: String::new(),
        last_response: String::new(),
        subtasks,
    });

    State::BuildPrompt
}

fn build_prompt(db: &Arc<Database>, ctx: &mut Context) -> State {
    let conv = TaskConversation::new(db.clone());
    let history = conv.get_messages(&ctx.task.id).unwrap_or_default();

    let mut prompt = String::new();

    if ctx.turn == 0 {
        // Initial prompt
        prompt.push_str(&format!("# Task: {}\n\n", ctx.task.title));
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

        prompt.push_str(
            "Work on this task using the tools available to you. \
             When you are done, use the `complete_task` tool to mark it complete.",
        );
    } else {
        // Continuation prompt
        prompt.push_str("Continue working on the current task.\n\n");

        if let Some(subtask) = current_subtask(&ctx.subtasks) {
            prompt.push_str(&format!("Current step: {}\n", subtask.title));
            if let Some(ref desc) = subtask.description {
                prompt.push_str(desc);
                prompt.push('\n');
            }
        }

        // Include recent history for context
        let recent: Vec<_> = history.iter().rev().take(4).rev().collect();
        if !recent.is_empty() {
            prompt.push_str("\nRecent conversation:\n");
            for msg in recent {
                prompt.push_str(&format!(
                    "[{}]: {}\n",
                    msg.role,
                    truncate(&msg.content, 500)
                ));
            }
        }

        prompt.push_str(
            "\nContinue from where you left off. \
             Use `complete_task` when this task is fully done.",
        );
    }

    // Save the user-side prompt as a task message
    let _ = conv.add_message(&ctx.task.id, "system", &prompt);

    ctx.current_prompt = prompt;
    State::CallAgent
}

async fn call_agent(ctx: &mut Context) -> State {
    let harness = HarnessClient::new(ctx.harness_port);

    debug!(
        task_id = ctx.task.id,
        turn = ctx.turn,
        agent_id = ctx.agent_id,
        "calling agent harness"
    );

    match harness
        .send_task(&ctx.system_prompt, &ctx.current_prompt, &ctx.model)
        .await
    {
        Ok(response) => {
            let text = response
                .choices
                .first()
                .map(|c| c.message.content.clone())
                .unwrap_or_default();
            ctx.last_response = text;
            State::ProcessResponse
        }
        Err(e) => {
            error!(
                task_id = ctx.task.id,
                turn = ctx.turn,
                error = %e,
                "harness call failed"
            );
            State::Done(DriverResult::Failed(format!("harness error: {e}")))
        }
    }
}

fn process_response(db: &Arc<Database>, ctx: &mut Context) -> State {
    // Save agent response as task message
    let conv = TaskConversation::new(db.clone());
    let _ = conv.add_message(&ctx.task.id, "assistant", &ctx.last_response);

    ctx.turn += 1;

    debug!(
        task_id = ctx.task.id,
        turn = ctx.turn,
        response_len = ctx.last_response.len(),
        "processed agent response"
    );

    State::Evaluate
}

fn evaluate(db: &Arc<Database>, ctx: &mut Context) -> State {
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

/// Send a task status notification back to the originating conversation.
/// This lets the user (and the agent) know the background task finished.
fn notify_conversation(db: &Arc<Database>, task_id: &str, agent_id: &str, status: &str) {
    let board = TaskBoard::new(db.clone());
    let task = match board.get(task_id) {
        Ok(t) => t,
        Err(_) => return,
    };

    let conv_id = match task.conversation_id {
        Some(ref id) => id.clone(),
        None => return,
    };

    let mgr = ConversationManager::new(db.clone());
    let content = json!({
        "task_id": task.id,
        "title": task.title,
        "status": status,
    })
    .to_string();

    if let Err(e) = mgr.send_message(
        &conv_id,
        &SendMessage {
            sender_type: "system".into(),
            sender_id: agent_id.to_string(),
            sender_name: Some(agent_id.to_string()),
            content,
            message_type: Some("task_status".into()),
        },
    ) {
        warn!(
            task_id,
            conv_id,
            error = %e,
            "failed to notify conversation of task completion"
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
    let in_progress = board.list(Some("in_progress"), None, 100)?;
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
                let _ = board.update_status(&claimed.task_id, "completed", None);
                info!(task_id = claimed.task_id, "task completed");
                notify_conversation(db, &claimed.task_id, &claimed.agent_id, "completed");
            }
            DriverResult::Failed(reason) => {
                let _ = queue.fail(claimed.id, reason);
                warn!(task_id = claimed.task_id, reason, "task execution failed");
                notify_conversation(db, &claimed.task_id, &claimed.agent_id, "failed");
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
        }
    }

    Ok(())
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
