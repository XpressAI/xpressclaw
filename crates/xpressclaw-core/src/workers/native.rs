//! Dispatcher and adapters for short-lived native coding-agent CLIs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use crate::config::{default_native_runner_image, AgentConfig, Config, NativeRunnerConfig};
use crate::conversations::event_bus::{ConversationEvent, ConversationEventBus};
use crate::conversations::{ConversationManager, SendMessage};
use crate::db::Database;
use crate::docker::manager::{ContainerSpec, DockerManager, VolumeMount};
use crate::error::{Error, Result};
use crate::sessions::SessionManager;
use crate::tasks::board::TaskBoard;
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::{QueueItem, TaskQueue};
use crate::workers::acp::{run_turn, AcpEventRecorder};

/// Consume the durable task queue as an Agent Client Protocol client. Each
/// queue item gets its own short-lived ACP server container and publishes
/// standard protocol events and artifacts to the logical session.
pub async fn start_dispatcher(
    db: Arc<Database>,
    config: Arc<RwLock<Arc<Config>>>,
    initial_docker: Option<Arc<DockerManager>>,
    event_bus: Arc<ConversationEventBus>,
) {
    info!("native attempt dispatcher started");
    let concurrency = Arc::new(Semaphore::new(4));
    let mut docker = initial_docker;

    loop {
        let docker = match docker.clone() {
            Some(docker) => docker,
            None => match DockerManager::connect().await {
                Ok(connected) => {
                    let connected = Arc::new(connected);
                    docker = Some(connected.clone());
                    connected
                }
                Err(_) => {
                    warn!("native dispatcher is waiting for Docker/Podman");
                    tokio::time::sleep(Duration::from_secs(15)).await;
                    continue;
                }
            },
        };

        let permit = match concurrency.clone().acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return,
        };
        let queue = TaskQueue::new(db.clone());
        match queue.claim_next() {
            Ok(Some(item)) => {
                let db = db.clone();
                let config = config.read().unwrap().clone();
                let event_bus = event_bus.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) =
                        execute_item(db.clone(), config, docker, event_bus.clone(), item.clone())
                            .await
                    {
                        error!(
                            queue_id = item.id,
                            task_id = item.task_id,
                            error = %error,
                            "native work attempt failed"
                        );
                        let _ = fail_item(&db, &item, &error.to_string(), &event_bus);
                    }
                });
            }
            Ok(None) => {
                drop(permit);
                tokio::time::sleep(Duration::from_millis(750)).await;
            }
            Err(error) => {
                drop(permit);
                warn!(error = %error, "failed to claim native work");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

async fn execute_item(
    db: Arc<Database>,
    config: Arc<Config>,
    docker: Arc<DockerManager>,
    event_bus: Arc<ConversationEventBus>,
    item: QueueItem,
) -> Result<()> {
    let attempt_id = item
        .attempt_id
        .as_deref()
        .ok_or_else(|| Error::Task(format!("queue item {} has no work attempt", item.id)))?;
    let agent = config
        .agents
        .iter()
        .find(|agent| agent.name == item.agent_id)
        .ok_or_else(|| Error::AgentNotFound {
            name: item.agent_id.clone(),
        })?;
    let kind = resolve_runner_kind(agent)?;
    let resume_session_id = resume_session_id(&db, &item, &kind)?;
    let prompt = build_prompt(&db, &item, attempt_id)?;
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE work_attempts SET runner = ?1, prompt = ?2 WHERE id = ?3",
            rusqlite::params![kind, prompt, attempt_id],
        )
    })?;

    let sessions = SessionManager::new(db.clone());
    sessions.transition_attempt(
        attempt_id,
        "preparing",
        &format!("Preparing {kind}"),
        None,
        None,
    )?;
    if let Some(conversation_id) = conversation_id(&db, &item.task_id) {
        event_bus.send(
            &conversation_id,
            ConversationEvent::Thinking {
                agent_id: item.agent_id.clone(),
            },
        );
    }
    let board = TaskBoard::new(db.clone());
    let _ = board.update_status(&item.task_id, "in_progress", Some(&item.agent_id));

    if let Some(native_session_id) = resume_session_id.as_deref() {
        sessions.set_native_session(attempt_id, native_session_id)?;
    }
    let mut spec = build_spec(&config, agent, &kind)?;
    let built_in_image = default_native_runner_image(&kind) == Some(spec.image.as_str());
    let image_ready = docker.has_image(&spec.image).await
        && (!built_in_image
            || docker
                .image_has_label(&spec.image, "io.xpressclaw.protocol", "acp")
                .await);
    if !image_ready {
        let local_fallback = match local_runner_image_alias(&spec.image) {
            Some(image)
                if docker.has_image(image).await
                    && docker
                        .image_has_label(image, "io.xpressclaw.protocol", "acp")
                        .await =>
            {
                Some(image)
            }
            _ => None,
        };
        if let Some(local_image) = local_fallback {
            spec.image = local_image.to_string();
        } else {
            sessions.transition_attempt(
                attempt_id,
                "preparing",
                &format!("Pulling {kind} runner image"),
                None,
                None,
            )?;
            docker.pull_image(&spec.image).await?;
            if built_in_image
                && !docker
                    .image_has_label(&spec.image, "io.xpressclaw.protocol", "acp")
                    .await
            {
                return Err(Error::Backend(format!(
                    "runner image {} predates the ACP integration; rebuild it from the current Dockerfile",
                    spec.image
                )));
            }
        }
    }
    let workload_id = format!("attempt-{attempt_id}");
    let attached = docker.launch_attached(&workload_id, &spec).await?;
    sessions.set_container(attempt_id, &attached.info.container_id)?;
    sessions.transition_attempt(
        attempt_id,
        "running",
        &format!("{kind} is working over ACP"),
        None,
        None,
    )?;

    let attempt = sessions.get_attempt(attempt_id)?;
    let recorder = AcpEventRecorder::new(
        db.clone(),
        attempt.session_id.clone(),
        attempt_id,
        item.task_id.clone(),
        kind.clone(),
    );
    let turn = run_turn(
        attached,
        recorder,
        resume_session_id.as_deref(),
        Path::new("/workspace"),
        &prompt,
        agent.runner.model.as_deref(),
    )
    .await;
    let _ = docker.stop(&workload_id).await;
    let turn = turn?;
    let current = sessions.get_attempt(attempt_id)?;
    if current.status == "cancelled" {
        return Ok(());
    }
    sessions.add_artifact(
        attempt_id,
        "runner_output",
        "ACP event transcript",
        Some(&turn.diagnostic),
        None,
        json!({ "protocol": "acp", "stop_reason": turn.stop_reason, "runner": kind }),
    )?;
    sessions.set_native_session(attempt_id, &turn.session_id)?;
    sessions.add_artifact(
        attempt_id,
        "result",
        "Attempt result",
        Some(&turn.summary),
        None,
        json!({ "protocol": "acp", "stop_reason": turn.stop_reason, "runner": kind }),
    )?;
    let completion_summary = truncate(&turn.summary, 2_000);
    sessions.transition_attempt(
        attempt_id,
        "completed",
        &completion_summary,
        Some(&turn.summary),
        None,
    )?;
    let queue = TaskQueue::new(db.clone());
    queue.complete(item.id, &turn.summary)?;
    if let Err(error) =
        TaskConversation::new(db.clone()).add_message(&item.task_id, "assistant", &turn.summary)
    {
        warn!(%error, task_id = item.task_id, "failed to persist ACP task reply");
    }

    let continuation_queued = queue.has_queued_for_task(&item.task_id)?;
    let waiting_for_user = needs_user_input(&turn.summary);
    let completed_tasks = if continuation_queued {
        board.update_status(&item.task_id, "in_progress", Some(&item.agent_id))?;
        Vec::new()
    } else if waiting_for_user {
        board.update_status(&item.task_id, "waiting_for_input", Some(&item.agent_id))?;
        Vec::new()
    } else if board.subtasks_complete(&item.task_id)? {
        board.complete_and_roll_up(&item.task_id, Some(&item.agent_id))?
    } else {
        board.update_status(&item.task_id, "in_progress", Some(&item.agent_id))?;
        Vec::new()
    };
    sessions.refresh_status(&item.agent_id)?;
    publish_conversation_result(
        &db,
        &event_bus,
        &item,
        &agent.context_label(),
        &turn.summary,
    );
    for completed in completed_tasks {
        advance_workflow(&db, &completed.id, "completed", &turn.summary);
    }
    Ok(())
}

fn fail_item(
    db: &Arc<Database>,
    item: &QueueItem,
    message: &str,
    event_bus: &Arc<ConversationEventBus>,
) -> Result<()> {
    let sessions = SessionManager::new(db.clone());
    if let Some(attempt_id) = item.attempt_id.as_deref() {
        if let Ok(attempt) = sessions.get_attempt(attempt_id) {
            // Cancellation updates the durable queue/task state before it
            // stops the container. The waiter may then observe a Docker error;
            // do not overwrite the user's cancellation with a failure.
            if attempt.status == "cancelled" {
                return Ok(());
            }
            let _ = sessions.transition_attempt(
                attempt_id,
                "failed",
                "Work attempt failed",
                None,
                Some(message),
            );
        }
    }
    let queue = TaskQueue::new(db.clone());
    queue.fail(item.id, message)?;
    let chat_message = format!(
        "The worker could not complete this turn.\n\n{}",
        truncate(message, 2_000)
    );
    if let Err(error) =
        TaskConversation::new(db.clone()).add_message(&item.task_id, "assistant", &chat_message)
    {
        warn!(%error, task_id = item.task_id, "failed to persist native task failure");
    }
    let board = TaskBoard::new(db.clone());
    let continuation_queued = queue.has_queued_for_task(&item.task_id).unwrap_or(false);
    let _ = board.update_status(
        &item.task_id,
        if continuation_queued {
            "in_progress"
        } else {
            "blocked"
        },
        Some(&item.agent_id),
    );
    let _ = sessions.refresh_status(&item.agent_id);
    if let Some(conversation_id) = conversation_id(db, &item.task_id) {
        event_bus.send(
            &conversation_id,
            ConversationEvent::Error {
                agent_id: Some(item.agent_id.clone()),
                error: message.to_string(),
            },
        );
        event_bus.send(&conversation_id, ConversationEvent::Done);
    }
    if !continuation_queued {
        advance_workflow(db, &item.task_id, "failed", message);
    }
    Ok(())
}

fn conversation_id(db: &Arc<Database>, task_id: &str) -> Option<String> {
    TaskBoard::new(db.clone())
        .get(task_id)
        .ok()
        .and_then(|task| task.conversation_id)
}

fn publish_conversation_result(
    db: &Arc<Database>,
    event_bus: &Arc<ConversationEventBus>,
    item: &QueueItem,
    sender_name: &str,
    content: &str,
) {
    let Some(conversation_id) = conversation_id(db, &item.task_id) else {
        return;
    };
    let manager = ConversationManager::new(db.clone());
    if let Ok(message) = manager.send_message(
        &conversation_id,
        &SendMessage {
            sender_type: "agent".to_string(),
            sender_id: item.agent_id.clone(),
            sender_name: Some(sender_name.to_string()),
            content: content.to_string(),
            message_type: None,
        },
    ) {
        event_bus.send(
            &conversation_id,
            ConversationEvent::Message {
                message: json!(message),
            },
        );
    }
    let _ = manager.mark_processed(&conversation_id);
    event_bus.send(&conversation_id, ConversationEvent::Done);
}

fn advance_workflow(db: &Arc<Database>, task_id: &str, status: &str, output: &str) {
    let engine = crate::workflows::engine::WorkflowEngine::new(db.clone());
    if engine
        .find_execution_by_task(task_id)
        .is_ok_and(|execution| execution.is_some())
    {
        if let Err(error) = engine.on_task_completed(task_id, status, output) {
            warn!(task_id, status, error = %error, "failed to advance workflow");
        }
    }
}

pub fn resolve_runner_kind(agent: &AgentConfig) -> Result<String> {
    if agent.runner.kind != "auto" {
        return Ok(agent.runner.kind.to_lowercase());
    }
    let backend = agent.backend.to_lowercase();
    if backend.contains("codex") {
        Ok("codex".to_string())
    } else if backend.contains("claude") {
        Ok("claude".to_string())
    } else if backend.contains("opencode") {
        Ok("opencode".to_string())
    } else if !agent.runner.command.is_empty() {
        Ok("custom".to_string())
    } else {
        Err(Error::Backend(format!(
            "session '{}' needs runner.kind or runner.command (backend was '{}')",
            agent.name, agent.backend
        )))
    }
}

fn build_prompt(db: &Arc<Database>, item: &QueueItem, attempt_id: &str) -> Result<String> {
    let task = TaskBoard::new(db.clone()).get(&item.task_id)?;
    let pending_user_messages: Vec<String> = db.with_conn(|conn| {
        let previous_started: Option<String> = conn
            .query_row(
                "SELECT started_at FROM work_attempts
                 WHERE task_id = ?1 AND id != ?2 AND started_at IS NOT NULL
                 ORDER BY created_at DESC LIMIT 1",
                rusqlite::params![item.task_id, attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut statement = conn.prepare(
            "SELECT content FROM task_messages
             WHERE task_id = ?1 AND role = 'user'
               AND (?2 IS NULL OR timestamp >= ?2)
             ORDER BY id ASC",
        )?;
        let messages = statement
            .query_map(rusqlite::params![item.task_id, previous_started], |row| {
                row.get(0)
            })?
            .filter_map(|row| row.ok())
            .collect();
        Ok::<_, Error>(messages)
    })?;
    if !pending_user_messages.is_empty() {
        return Ok(pending_user_messages.join("\n\n"));
    }

    let description = task
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty());
    let from_project_composer = task
        .context
        .as_ref()
        .and_then(|context| context.get("origin"))
        .and_then(Value::as_str)
        == Some("session_message");
    Ok(match (description, from_project_composer) {
        (Some(description), true) => description.to_string(),
        (Some(description), false) => format!("{}\n\n{}", task.title, description),
        (None, _) => task.title,
    })
}

/// Pick the ACP conversation for this task turn. Explicit task
/// dependencies are strongest, followed by an existing turn on the same task.
/// A task marked `session_mode: new` then starts clean; all other work resumes
/// the latest conversation in the project.
fn resume_session_id(db: &Arc<Database>, item: &QueueItem, runner: &str) -> Result<Option<String>> {
    let board = TaskBoard::new(db.clone());
    let task = board.get(&item.task_id)?;
    let dependency_session = db.with_conn(|conn| {
        conn.query_row(
            "SELECT a.native_session_id
             FROM task_dependencies d
             JOIN work_attempts a ON a.task_id = d.depends_on_id
             WHERE d.task_id = ?1 AND a.session_id = ?2 AND a.runner = ?3
               AND a.native_session_id IS NOT NULL AND a.status != 'cancelled'
             ORDER BY COALESCE(a.completed_at, a.created_at) DESC LIMIT 1",
            rusqlite::params![item.task_id, item.agent_id, runner],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if dependency_session.is_some() {
        return Ok(dependency_session);
    }

    let task_session = db.with_conn(|conn| {
        conn.query_row(
            "SELECT native_session_id FROM work_attempts
             WHERE task_id = ?1 AND session_id = ?2 AND runner = ?3
               AND native_session_id IS NOT NULL AND status != 'cancelled'
             ORDER BY COALESCE(completed_at, created_at) DESC LIMIT 1",
            rusqlite::params![item.task_id, item.agent_id, runner],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if task_session.is_some() {
        return Ok(task_session);
    }

    let start_new = task
        .context
        .as_ref()
        .and_then(|context| context.get("session_mode"))
        .and_then(Value::as_str)
        == Some("new");
    if start_new {
        return Ok(None);
    }

    db.with_conn(|conn| {
        conn.query_row(
            "SELECT native_session_id FROM work_attempts
             WHERE session_id = ?1 AND runner = ?2
               AND native_session_id IS NOT NULL AND status != 'cancelled'
             ORDER BY COALESCE(completed_at, created_at) DESC LIMIT 1",
            rusqlite::params![item.agent_id, runner],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })
}

fn build_spec(config: &Config, agent: &AgentConfig, kind: &str) -> Result<ContainerSpec> {
    let command = acp_command_for(&agent.runner, kind)?;
    let image = resolved_runner_image(&agent.runner, kind)?;
    let workspace = agent
        .runner
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(expand_home)
        .map(PathBuf::from)
        .unwrap_or_else(|| config.system.workspace_dir.clone());
    let workspace = canonical_or_original(&workspace);
    let mut volumes = vec![VolumeMount {
        source: workspace.display().to_string(),
        target: "/workspace".to_string(),
        read_only: false,
    }];
    for volume in &agent.volumes {
        if let Some(mount) = parse_volume(volume) {
            volumes.push(mount);
        }
    }
    if agent.runner.subscription_auth {
        volumes.extend(auth_mounts(kind));
    }

    Ok(ContainerSpec {
        image,
        memory_limit: Some(4 * 1024 * 1024 * 1024),
        cpu_limit: None,
        environment: vec![
            "HOME=/home/node".to_string(),
            "CI=1".to_string(),
            "NO_COLOR=1".to_string(),
        ],
        volumes,
        network_mode: Some("bridge".to_string()),
        expose_port: None,
        cmd: Some(command),
        working_dir: Some("/workspace".to_string()),
        run_as_host_user: true,
    })
}

pub fn resolved_runner_image(config: &NativeRunnerConfig, kind: &str) -> Result<String> {
    if config.image.trim().is_empty() {
        return default_native_runner_image(kind)
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::Backend(format!(
                    "runner '{kind}' requires an explicit container image"
                ))
            });
    }
    Ok(config.image.trim().to_string())
}

/// Local tags used by the ACP runner images. A published image is the
/// default, but retaining these aliases lets existing developer builds run
/// without a forced retag or registry pull.
pub fn local_runner_image_alias(image: &str) -> Option<&'static str> {
    match image {
        "ghcr.io/xpressai/xpressclaw-runner-codex:latest" => Some("xpressclaw-runner-codex:latest"),
        "ghcr.io/xpressai/xpressclaw-runner-claude:latest" => {
            Some("xpressclaw-runner-claude:latest")
        }
        "ghcr.io/xpressai/xpressclaw-runner-opencode:latest" => {
            Some("xpressclaw-runner-opencode:latest")
        }
        _ => None,
    }
}

fn acp_command_for(config: &NativeRunnerConfig, kind: &str) -> Result<Vec<String>> {
    if !config.command.is_empty() {
        return Ok(config
            .command
            .iter()
            .map(|part| part.replace("{workspace}", "/workspace"))
            .collect());
    }
    match kind {
        "codex" => Ok(vec!["codex-acp".into()]),
        "claude" => Ok(vec!["claude-agent-acp".into()]),
        "opencode" => Ok(vec!["opencode".into(), "acp".into()]),
        _ => Err(Error::Backend(format!(
            "ACP runner '{kind}' requires an explicit server command"
        ))),
    }
}

fn auth_candidates(kind: &str) -> Vec<(PathBuf, &'static str, bool)> {
    let Some(home) = host_home() else {
        return Vec::new();
    };
    match kind {
        "codex" => vec![(home.join(".codex"), "/home/node/.codex", false)],
        "claude" => vec![
            (home.join(".claude"), "/home/node/.claude", false),
            (home.join(".claude.json"), "/home/node/.claude.json", false),
        ],
        "opencode" => vec![
            (
                home.join(".local/share/opencode"),
                "/home/node/.local/share/opencode",
                false,
            ),
            (
                home.join(".config/opencode"),
                "/home/node/.config/opencode",
                false,
            ),
        ],
        _ => Vec::new(),
    }
}

/// Whether the host has a standard login location that can be mounted for the
/// selected agent product. This intentionally reports only presence, never
/// credential contents.
pub fn subscription_auth_available(kind: &str) -> bool {
    auth_candidates(kind)
        .iter()
        .any(|(source, _, _)| source.exists())
}

fn auth_mounts(kind: &str) -> Vec<VolumeMount> {
    let Some(home) = host_home() else {
        return Vec::new();
    };
    let mut candidates = auth_candidates(kind);
    // Git identity and `gh` authentication let coding workflows push or mark
    // a PR ready without copying credentials into the image. SSH keys are not
    // mounted implicitly; users can opt in with an explicit volume.
    candidates.extend([
        (home.join(".gitconfig"), "/home/node/.gitconfig", true),
        (home.join(".config/gh"), "/home/node/.config/gh", true),
    ]);
    candidates
        .into_iter()
        .filter(|(source, _, _)| source.exists())
        .map(|(source, target, read_only)| VolumeMount {
            source: source.display().to_string(),
            target: target.to_string(),
            // Native agent OAuth directories must be writable so their CLIs
            // can refresh credentials; common Git/GitHub config is read-only.
            read_only,
        })
        .collect()
}

fn host_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn parse_volume(raw: &str) -> Option<VolumeMount> {
    let (source_or_target, target_or_mode) = raw.rsplit_once(':')?;
    if target_or_mode == "ro" || target_or_mode == "rw" {
        let (source, target) = source_or_target.rsplit_once(':')?;
        return Some(VolumeMount {
            source: expand_home(source),
            target: target.to_string(),
            read_only: target_or_mode == "ro",
        });
    }
    Some(VolumeMount {
        source: expand_home(source_or_target),
        target: target_or_mode.to_string(),
        read_only: false,
    })
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = host_home() {
            return home.join(rest).display().to_string();
        }
    }
    path.to_string()
}

fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn needs_user_input(summary: &str) -> bool {
    if summary.lines().any(|line| {
        line.trim_start()
            .trim_start_matches(['`', '*', '-', '#', ' '])
            .starts_with("NEEDS_USER_INPUT:")
    }) {
        return true;
    }

    // Native products do not share a structured "ask the user" event. Treat
    // a final, direct question as waiting without requiring XpressClaw to
    // rewrite the user's task or inject its own agent protocol.
    let last_line = summary
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .trim_matches(['`', '*', '_', '#', ' ']);
    if !last_line.ends_with('?') {
        return false;
    }
    let question = last_line.to_lowercase();
    [
        "which ",
        "what ",
        "where ",
        "when ",
        "how ",
        "can you ",
        "could you ",
        "would you ",
        "should i ",
        "do you ",
        "please ",
        "i need ",
    ]
    .iter()
    .any(|signal| question.starts_with(signal) || question.contains(&format!(" {signal}")))
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max_chars).collect();
    truncated.push_str("\n… output truncated …");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_legacy_claude_backend_to_native_cli() {
        let agent = AgentConfig {
            backend: "claude-sdk".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_runner_kind(&agent).unwrap(), "claude");
    }

    #[test]
    fn recognizes_explicit_native_questions() {
        assert!(needs_user_input(
            "I need one decision.\n\nNEEDS_USER_INPUT: Which database should I use?"
        ));
        assert!(needs_user_input(
            "**NEEDS_USER_INPUT: Which database should I use?**"
        ));
        assert!(needs_user_input(
            "I can continue once you decide.\n\nWhich database should I use?"
        ));
        assert!(needs_user_input(
            "The options are ready. Would you like the compact layout?"
        ));
        assert!(!needs_user_input("Implemented and tested. No blockers."));
        assert!(!needs_user_input("Implemented the requested FAQ page."));
    }

    #[test]
    fn expands_custom_command_placeholders() {
        let config = NativeRunnerConfig {
            command: vec!["runner".into(), "--cwd={workspace}".into()],
            ..Default::default()
        };
        assert_eq!(
            acp_command_for(&config, "custom").unwrap(),
            vec!["runner", "--cwd=/workspace"]
        );
    }

    #[test]
    fn starts_the_builtin_acp_servers() {
        let config = NativeRunnerConfig::default();
        assert_eq!(
            acp_command_for(&config, "codex").unwrap(),
            vec!["codex-acp"]
        );
        assert_eq!(
            acp_command_for(&config, "claude").unwrap(),
            vec!["claude-agent-acp"]
        );
        assert_eq!(
            acp_command_for(&config, "opencode").unwrap(),
            vec!["opencode", "acp"]
        );
    }

    #[test]
    fn selects_project_dependency_and_fresh_conversation_contexts() {
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        let board = TaskBoard::new(db.clone());
        SessionManager::new(db.clone())
            .ensure("atlas", Some("atlas"))
            .unwrap();
        let first = board
            .create(&CreateTask {
                title: "First turn".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        let first_item = queue.enqueue(&first.id, "atlas").unwrap();
        let first_attempt = first_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex', status = 'completed',
                    completed_at = CURRENT_TIMESTAMP WHERE id = ?1",
                [first_attempt],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .set_native_session(first_attempt, "thread-1")
            .unwrap();

        let regular = board
            .create(&CreateTask {
                title: "Continue project".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let regular_item = queue.enqueue(&regular.id, "atlas").unwrap();
        assert_eq!(
            resume_session_id(&db, &regular_item, "codex").unwrap(),
            Some("thread-1".into())
        );

        let fresh = board
            .create(&CreateTask {
                title: "Start clean".into(),
                agent_id: Some("atlas".into()),
                context: Some(json!({ "session_mode": "new" })),
                ..Default::default()
            })
            .unwrap();
        let fresh_item = queue.enqueue(&fresh.id, "atlas").unwrap();
        assert_eq!(resume_session_id(&db, &fresh_item, "codex").unwrap(), None);

        let dependent = board
            .create(&CreateTask {
                title: "Continue prerequisite".into(),
                agent_id: Some("atlas".into()),
                context: Some(json!({ "session_mode": "new" })),
                ..Default::default()
            })
            .unwrap();
        board.add_dependency(&dependent.id, &first.id).unwrap();
        let dependent_item = queue.enqueue(&dependent.id, "atlas").unwrap();
        assert_eq!(
            resume_session_id(&db, &dependent_item, "codex").unwrap(),
            Some("thread-1".into())
        );
    }

    #[test]
    fn selects_a_minimal_image_for_each_native_runner() {
        assert_eq!(
            default_native_runner_image("codex"),
            Some("ghcr.io/xpressai/xpressclaw-runner-codex:latest")
        );
        assert_eq!(
            default_native_runner_image("claude"),
            Some("ghcr.io/xpressai/xpressclaw-runner-claude:latest")
        );
        assert_eq!(
            default_native_runner_image("opencode"),
            Some("ghcr.io/xpressai/xpressclaw-runner-opencode:latest")
        );
        assert_eq!(default_native_runner_image("custom"), None);
    }

    #[test]
    fn parses_read_only_and_read_write_volume_mounts() {
        let read_only = parse_volume("/tmp/reference:/workspace/reference:ro").unwrap();
        assert_eq!(read_only.source, "/tmp/reference");
        assert_eq!(read_only.target, "/workspace/reference");
        assert!(read_only.read_only);

        let read_write = parse_volume("/tmp/project:/workspace/project").unwrap();
        assert_eq!(read_write.source, "/tmp/project");
        assert_eq!(read_write.target, "/workspace/project");
        assert!(!read_write.read_only);
    }

    #[test]
    fn published_images_keep_the_prototype_local_aliases() {
        assert_eq!(
            local_runner_image_alias("ghcr.io/xpressai/xpressclaw-runner-codex:latest"),
            Some("xpressclaw-runner-codex:latest")
        );
        assert_eq!(local_runner_image_alias("example/custom:latest"), None);
    }

    #[test]
    fn cancellation_is_not_overwritten_by_container_exit_error() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tasks (id, title, agent_id) VALUES ('task-1', 'Cancel me', 'atlas')",
                [],
            )
            .unwrap();
        });
        let queue = TaskQueue::new(db.clone());
        let item = queue.enqueue("task-1", "atlas").unwrap();
        let attempt_id = item.attempt_id.as_deref().unwrap();
        let sessions = SessionManager::new(db.clone());
        sessions
            .transition_attempt(attempt_id, "cancelled", "Cancelled", None, None)
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_queue SET status = 'failed', harness_response = 'cancelled by user' WHERE id = ?1",
                [item.id],
            )?;
            conn.execute("UPDATE tasks SET status = 'cancelled' WHERE id = 'task-1'", [])?;
            Ok::<_, Error>(())
        })
        .unwrap();

        fail_item(
            &db,
            &item,
            "container disappeared",
            &Arc::new(ConversationEventBus::new()),
        )
        .unwrap();

        assert_eq!(
            sessions.get_attempt(attempt_id).unwrap().status,
            "cancelled"
        );
        assert_eq!(
            TaskBoard::new(db.clone())
                .get("task-1")
                .unwrap()
                .status
                .as_str(),
            "cancelled"
        );
        assert_eq!(
            queue.get(item.id).unwrap().harness_response.as_deref(),
            Some("cancelled by user")
        );
    }

    #[test]
    fn native_results_return_to_conversation_history() {
        let db = Arc::new(Database::open_memory().unwrap());
        let conversations = ConversationManager::new(db.clone());
        let conversation = conversations
            .create(&crate::conversations::CreateConversation {
                title: Some("Native session".to_string()),
                icon: None,
                participant_ids: vec!["atlas".to_string()],
            })
            .unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&crate::tasks::board::CreateTask {
                title: "Reply".to_string(),
                description: Some("Reply from the native worker".to_string()),
                agent_id: Some("atlas".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: Some(conversation.id.clone()),
                priority: None,
                context: None,
            })
            .unwrap();
        let item = TaskQueue::new(db.clone())
            .enqueue(&task.id, "atlas")
            .unwrap();

        publish_conversation_result(
            &db,
            &Arc::new(ConversationEventBus::new()),
            &item,
            "Atlas",
            "Native result",
        );

        let messages = conversations
            .get_messages(&conversation.id, 10, None)
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].sender_type, "agent");
        assert_eq!(messages[0].content, "Native result");
    }
}
