//! Dispatcher and adapters for short-lived native coding-agent CLIs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use crate::config::{default_native_runner_image, AgentConfig, Config, NativeRunnerConfig};
use crate::conversations::event_bus::{ConversationEvent, ConversationEventBus};
use crate::conversations::{ConversationManager, SendMessage};
use crate::db::Database;
use crate::docker::manager::{ContainerOutput, ContainerSpec, DockerManager, VolumeMount};
use crate::error::{Error, Result};
use crate::sessions::{NewEvent, SessionManager};
use crate::tasks::board::TaskBoard;
use crate::tasks::queue::{QueueItem, TaskQueue};

const MAX_CAPTURED_OUTPUT: usize = 200_000;

#[derive(Debug)]
struct NativeResult {
    summary: String,
    native_session_id: Option<String>,
    progress: Vec<(String, Value)>,
}

/// Consume the durable task queue with native CLIs instead of a bespoke agent
/// loop. Each queue item gets its own short-lived container and publishes only
/// structured events/artifacts to the logical session.
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
    let prompt = build_prompt(&db, agent, &item, attempt_id)?;
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

    let spec = build_spec(&config, agent, &kind, &prompt)?;
    if !docker.has_image(&spec.image).await {
        sessions.transition_attempt(
            attempt_id,
            "preparing",
            &format!("Pulling {kind} runner image"),
            None,
            None,
        )?;
        docker.pull_image(&spec.image).await?;
    }
    let workload_id = format!("attempt-{attempt_id}");
    let container = docker.launch(&workload_id, &spec).await?;
    sessions.set_container(attempt_id, &container.container_id)?;
    sessions.transition_attempt(
        attempt_id,
        "running",
        &format!("{kind} is working"),
        None,
        None,
    )?;

    let output = docker.wait_for_exit(&workload_id).await?;
    let current = sessions.get_attempt(attempt_id)?;
    if current.status == "cancelled" {
        return Ok(());
    }
    let captured = truncate(&output.output, MAX_CAPTURED_OUTPUT);
    sessions.add_artifact(
        attempt_id,
        "runner_output",
        "Native runner event stream",
        Some(&captured),
        None,
        json!({ "status_code": output.status_code, "runner": kind }),
    )?;

    if output.status_code != 0 {
        return Err(Error::Backend(format!(
            "{kind} exited with status {}: {}",
            output.status_code,
            tail(&captured, 2_000)
        )));
    }

    let parsed = parse_output(&kind, &output);
    if let Some(native_session_id) = parsed.native_session_id.as_deref() {
        sessions.set_native_session(attempt_id, native_session_id)?;
    }
    let attempt = sessions.get_attempt(attempt_id)?;
    for (summary, payload) in parsed.progress.into_iter().take(50) {
        sessions.append_event(
            &attempt.session_id,
            NewEvent {
                attempt_id: Some(attempt_id),
                task_id: Some(&item.task_id),
                source_type: "runner",
                source_id: Some(&kind),
                event_type: "runner_progress",
                summary: &summary,
                payload,
            },
        )?;
    }
    sessions.add_artifact(
        attempt_id,
        "result",
        "Attempt result",
        Some(&parsed.summary),
        None,
        json!({ "runner": kind }),
    )?;
    let completion_summary = truncate(&parsed.summary, 2_000);
    sessions.transition_attempt(
        attempt_id,
        "completed",
        &completion_summary,
        Some(&parsed.summary),
        None,
    )?;
    TaskQueue::new(db.clone()).complete(item.id, &parsed.summary)?;
    board.update_status(&item.task_id, "completed", Some(&item.agent_id))?;
    publish_conversation_result(
        &db,
        &event_bus,
        &item,
        agent.display_name.as_deref().unwrap_or(&agent.name),
        &parsed.summary,
    );
    advance_workflow(&db, &item.task_id, "completed", &parsed.summary);
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
    TaskQueue::new(db.clone()).fail(item.id, message)?;
    let board = TaskBoard::new(db.clone());
    let _ = board.update_status(&item.task_id, "blocked", Some(&item.agent_id));
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
    advance_workflow(db, &item.task_id, "failed", message);
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
            "profile '{}' needs runner.kind or runner.command (backend was '{}')",
            agent.name, agent.backend
        )))
    }
}

fn build_prompt(
    db: &Arc<Database>,
    agent: &AgentConfig,
    item: &QueueItem,
    attempt_id: &str,
) -> Result<String> {
    let task = TaskBoard::new(db.clone()).get(&item.task_id)?;
    let sessions = SessionManager::new(db.clone());
    let events = sessions.list_events(&item.agent_id, None, 30)?;
    let messages: Vec<(String, String)> = db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT role, content FROM task_messages WHERE task_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt
            .query_map([&item.task_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|row| row.ok())
            .collect();
        Ok::<_, Error>(rows)
    })?;

    let mut prompt = String::new();
    let role = agent.full_system_prompt();
    if !role.trim().is_empty() {
        prompt.push_str("Profile instructions:\n");
        prompt.push_str(&role);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&format!(
        "You are executing work attempt {attempt_id} inside logical session {}.\n",
        item.agent_id
    ));
    prompt.push_str("Work autonomously in /workspace. Return a concise result describing outcomes, changed files, verification, and any blocker. Do not ask the user to watch a terminal.\n\n");
    prompt.push_str("Task:\n");
    prompt.push_str(&task.title);
    if let Some(description) = task.description.as_deref() {
        if !description.trim().is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(description);
        }
    }
    if !events.is_empty() {
        prompt.push_str("\n\nRecent session activity (oldest to newest):\n");
        for event in events {
            if event.attempt_id.as_deref() != Some(attempt_id) {
                prompt.push_str(&format!(
                    "- [{}:{}] {}\n",
                    event.source_type, event.event_type, event.summary
                ));
            }
        }
    }
    if !messages.is_empty() {
        prompt.push_str("\nTask conversation:\n");
        for (role, content) in messages {
            prompt.push_str(&format!("- {role}: {content}\n"));
        }
    }
    Ok(prompt)
}

fn build_spec(
    config: &Config,
    agent: &AgentConfig,
    kind: &str,
    prompt: &str,
) -> Result<ContainerSpec> {
    let command = command_for(&agent.runner, kind, prompt)?;
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

fn command_for(config: &NativeRunnerConfig, kind: &str, prompt: &str) -> Result<Vec<String>> {
    if !config.command.is_empty() {
        return Ok(config
            .command
            .iter()
            .map(|part| {
                part.replace("{prompt}", prompt)
                    .replace("{workspace}", "/workspace")
            })
            .collect());
    }
    match kind {
        "codex" => Ok(vec![
            "codex".into(),
            "exec".into(),
            "--json".into(),
            "--dangerously-bypass-approvals-and-sandbox".into(),
            "--skip-git-repo-check".into(),
            "-C".into(),
            "/workspace".into(),
            prompt.into(),
        ]),
        "claude" => Ok(vec![
            "claude".into(),
            "-p".into(),
            prompt.into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--dangerously-skip-permissions".into(),
            "--max-turns".into(),
            config.max_turns.to_string(),
        ]),
        "opencode" => Ok(vec![
            "opencode".into(),
            "run".into(),
            "--format".into(),
            "json".into(),
            prompt.into(),
        ]),
        _ => Err(Error::Backend(format!(
            "runner '{kind}' requires an explicit command"
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
/// selected native product. This intentionally reports only presence, never
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
    let mut parts = raw.rsplitn(2, ':');
    let target_or_mode = parts.next()?;
    let source_or_target = parts.next()?;
    if target_or_mode == "ro" || target_or_mode == "rw" {
        let mut remaining = source_or_target.rsplitn(2, ':');
        let target = remaining.next()?;
        let source = remaining.next()?;
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

fn parse_output(kind: &str, output: &ContainerOutput) -> NativeResult {
    let mut summary = String::new();
    let mut native_session_id = None;
    let mut progress = Vec::new();

    for line in output.output.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match kind {
            "codex" => match value.get("type").and_then(Value::as_str) {
                Some("thread.started") => {
                    native_session_id = value
                        .get("thread_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                Some("item.completed") => {
                    if let Some(item) = value.get("item") {
                        match item.get("type").and_then(Value::as_str) {
                            Some("agent_message") => {
                                if let Some(text) = item.get("text").and_then(Value::as_str) {
                                    summary = text.to_string();
                                }
                            }
                            Some("command_execution") => {
                                let command = item
                                    .get("command")
                                    .and_then(Value::as_str)
                                    .unwrap_or("command");
                                progress.push((
                                    format!("Ran {}", truncate(command, 160)),
                                    json!({ "item_type": "command_execution" }),
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
            "claude" => match value.get("type").and_then(Value::as_str) {
                Some("system") => {
                    native_session_id = value
                        .get("session_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                Some("result") => {
                    if let Some(text) = value.get("result").and_then(Value::as_str) {
                        summary = text.to_string();
                    }
                    if native_session_id.is_none() {
                        native_session_id = value
                            .get("session_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                    }
                }
                _ => {}
            },
            _ => {
                if let Some(text) = value
                    .get("result")
                    .or_else(|| value.get("text"))
                    .and_then(Value::as_str)
                {
                    summary = text.to_string();
                }
            }
        }
    }
    if summary.trim().is_empty() {
        summary = tail(&output.output, 8_000);
    }
    NativeResult {
        summary,
        native_session_id,
        progress,
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max_chars).collect();
    truncated.push_str("\n… output truncated …");
    truncated
}

fn tail(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        value.to_string()
    } else {
        chars[chars.len() - max_chars..].iter().collect()
    }
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
    fn parses_codex_jsonl_without_exposing_terminal_protocol() {
        let output = ContainerOutput {
            status_code: 0,
            output: concat!(
                "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"command\":\"cargo test\"}}\n",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Implemented and tested.\"}}\n"
            )
            .to_string(),
        };
        let result = parse_output("codex", &output);
        assert_eq!(result.native_session_id.as_deref(), Some("thread-1"));
        assert_eq!(result.summary, "Implemented and tested.");
        assert_eq!(result.progress[0].0, "Ran cargo test");
    }

    #[test]
    fn expands_custom_command_placeholders() {
        let config = NativeRunnerConfig {
            command: vec![
                "runner".into(),
                "--cwd={workspace}".into(),
                "{prompt}".into(),
            ],
            ..Default::default()
        };
        assert_eq!(
            command_for(&config, "custom", "do work").unwrap(),
            vec!["runner", "--cwd=/workspace", "do work"]
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
