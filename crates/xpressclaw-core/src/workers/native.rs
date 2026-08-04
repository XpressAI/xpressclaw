//! Dispatcher and adapters for native coding-agent CLIs running in retained
//! project environments.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio,
};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use tracing::{error, info, warn};

use crate::acp::{agent_definition, local_runner_image};
use crate::config::{
    default_native_runner_image, AgentConfig, Config, ContainerEngineAccess, McpServerConfig,
    NativeRunnerConfig,
};
use crate::conversations::event_bus::{ConversationEvent, ConversationEventBus};
use crate::conversations::{ConversationManager, SendMessage};
use crate::db::Database;
use crate::docker::manager::{
    container_spec_fingerprint, ContainerSpec, DockerManager, VolumeMount,
};
use crate::error::{Error, Result};
use crate::sessions::SessionManager;
use crate::tasks::board::{Task, TaskBoard};
use crate::tasks::conversation::{PromptImageAttachment, TaskConversation};
use crate::tasks::queue::{QueueItem, TaskQueue};
use crate::workers::acp::{
    AcpElicitationBroker, AcpEventRecorder, AcpProcess, AcpSessionStart, AcpTurnControlBroker,
    AcpTurnOptions, AcpTurnRuntime,
};
use crate::workers::github;

const BUILT_IN_RUNNER_PROTOCOL: &str = "acp-xpressclaw-v2";
const BUNDLED_CONTROL_MCP_COMMAND: &str = "/usr/local/bin/node";
const BUNDLED_CONTROL_MCP_SOURCE: &str = concat!(
    include_str!("../../../../harnesses/native/common/mcp-xpressclaw.mjs"),
    "\nawait main();\n"
);

struct NativeAttemptRuntime {
    db: Arc<Database>,
    config: Arc<Config>,
    docker: Arc<DockerManager>,
    event_bus: Arc<ConversationEventBus>,
    elicitation_broker: Arc<AcpElicitationBroker>,
    turn_controls: Arc<AcpTurnControlBroker>,
    processes: Arc<ProjectAcpProcesses>,
    control_plane_port: u16,
}

#[derive(Clone)]
struct ProjectAcpProcess {
    fingerprint: String,
    container_id: String,
    process: AcpProcess,
}

#[derive(Default)]
struct ProjectAcpProcesses {
    slots: StdMutex<HashMap<String, Arc<AsyncMutex<Option<ProjectAcpProcess>>>>>,
}

impl ProjectAcpProcesses {
    fn slot(&self, agent_id: &str) -> Arc<AsyncMutex<Option<ProjectAcpProcess>>> {
        self.slots
            .lock()
            .unwrap()
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
            .clone()
    }

    async fn get_or_start(
        &self,
        docker: &DockerManager,
        agent_id: &str,
        spec: &ContainerSpec,
    ) -> Result<ProjectAcpProcess> {
        let fingerprint = container_spec_fingerprint(spec)?;
        let slot = self.slot(agent_id);
        let mut entry = slot.lock().await;
        let reusable = if let Some(current) = entry.as_ref() {
            current.fingerprint == fingerprint
                && current.process.is_alive()
                && docker.is_running(agent_id).await
                && docker.project_container_matches(agent_id, spec).await
        } else {
            false
        };
        if reusable {
            return Ok(entry.as_ref().unwrap().clone());
        }

        let previous = entry.take();
        docker.stop_preserving(agent_id).await?;
        if let Some(previous) = previous {
            let _ = tokio::time::timeout(Duration::from_secs(2), previous.process.wait_for_exit())
                .await;
        }
        let attached = docker.launch_project_attached(agent_id, spec).await?;
        let container_id = attached.info.container_id.clone();
        let process = match AcpProcess::start(attached).await {
            Ok(process) => process,
            Err(error) => {
                let _ = docker.stop_preserving(agent_id).await;
                return Err(error);
            }
        };
        let started = ProjectAcpProcess {
            fingerprint,
            container_id,
            process,
        };
        *entry = Some(started.clone());
        Ok(started)
    }

    async fn invalidate(&self, agent_id: &str, process: &AcpProcess) -> bool {
        let slot = self.slot(agent_id);
        let mut entry = slot.lock().await;
        if entry
            .as_ref()
            .is_some_and(|current| current.process.same_process(process))
        {
            entry.take();
            true
        } else {
            false
        }
    }
}

/// Consume the durable task queue as an Agent Client Protocol client. Each
/// project gets one retained container and initialized ACP process that are
/// reused across ordinary prompt turns.
pub async fn start_dispatcher(
    db: Arc<Database>,
    config: Arc<RwLock<Arc<Config>>>,
    initial_docker: Option<Arc<DockerManager>>,
    event_bus: Arc<ConversationEventBus>,
    elicitation_broker: Arc<AcpElicitationBroker>,
    turn_controls: Arc<AcpTurnControlBroker>,
    control_plane_port: u16,
) {
    info!("native attempt dispatcher started");
    let concurrency = Arc::new(Semaphore::new(4));
    let processes = Arc::new(ProjectAcpProcesses::default());
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
                let elicitation_broker = elicitation_broker.clone();
                let turn_controls = turn_controls.clone();
                let processes = processes.clone();
                if let Some(attempt_id) = item.attempt_id.as_deref() {
                    turn_controls.begin_attempt(attempt_id);
                }
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = execute_item(
                        NativeAttemptRuntime {
                            db: db.clone(),
                            config,
                            docker,
                            event_bus: event_bus.clone(),
                            elicitation_broker,
                            turn_controls: turn_controls.clone(),
                            processes,
                            control_plane_port,
                        },
                        item.clone(),
                    )
                    .await;
                    if let Some(attempt_id) = item.attempt_id.as_deref() {
                        turn_controls.finish_attempt(attempt_id);
                    }
                    if let Err(error) = result {
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

async fn execute_item(runtime: NativeAttemptRuntime, item: QueueItem) -> Result<()> {
    let NativeAttemptRuntime {
        db,
        config,
        docker,
        event_bus,
        elicitation_broker,
        turn_controls,
        processes,
        control_plane_port,
    } = runtime;
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
    let session_start = session_start(&db, &item, &kind)?;
    let requested_session_config = requested_session_config(&db, agent, &item.task_id)?;
    let mut prompt = build_prompt(&db, &item, attempt_id)?;
    if session_start == AcpSessionStart::New {
        prepend_unresumed_interrupted_prompt(&db, &item, attempt_id, &mut prompt)?;
    }
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE work_attempts SET runner = ?1, prompt = ?2 WHERE id = ?3",
            rusqlite::params![kind, prompt.content, attempt_id],
        )
    })?;

    let sessions = SessionManager::new(db.clone());
    let preparing = sessions.transition_attempt(
        attempt_id,
        "preparing",
        &format!("Preparing {kind}"),
        None,
        None,
    )?;
    if attempt_is_terminal(&preparing.status) {
        return Ok(());
    }
    if let Some(conversation_id) = conversation_id(&db, &item.task_id) {
        event_bus.send(
            &conversation_id,
            ConversationEvent::Thinking {
                agent_id: item.agent_id.clone(),
            },
        );
    }
    let board = TaskBoard::new(db.clone());
    let task = board.get(&item.task_id)?;
    let control_task_id = continuation_task_id(&task).map(str::to_owned);
    let _ = board.update_status(&item.task_id, "in_progress", Some(&item.agent_id));

    if let AcpSessionStart::Resume(native_session_id) = &session_start {
        sessions.set_native_session(attempt_id, native_session_id)?;
    }
    let workspace = resolved_workspace(&config, agent);
    let github = github::discover(&db, &workspace);
    let mut spec = build_spec(&config, agent, &kind, &docker, github.as_ref())?;
    let container_workspace = spec
        .working_dir
        .clone()
        .unwrap_or_else(|| "/workspace".to_string());
    let built_in_image = default_native_runner_image(&kind, agent.runner.container_engine)
        == Some(spec.image.as_str());
    let image_ready = runner_image_ready(&docker, &spec.image, built_in_image, agent).await;
    if !image_ready {
        let local_fallback = match local_runner_image_alias(&spec.image) {
            Some(image) if runner_image_ready(&docker, image, built_in_image, agent).await => {
                Some(image)
            }
            _ => None,
        };
        if let Some(local_image) = local_fallback {
            spec.image = local_image.to_string();
        } else {
            let pulling = sessions.transition_attempt(
                attempt_id,
                "preparing",
                &format!("Pulling {kind} runner image"),
                None,
                None,
            )?;
            if attempt_is_terminal(&pulling.status) {
                return Ok(());
            }
            docker.pull_image(&spec.image).await?;
            if !runner_image_ready(&docker, &spec.image, built_in_image, agent).await {
                return Err(Error::Backend(format!(
                    "runner image {} is incompatible with the configured ACP or container-engine mode; rebuild it from the current Dockerfile",
                    spec.image
                )));
            }
        }
    }
    if attempt_is_terminal(&sessions.get_attempt(attempt_id)?.status) {
        return Ok(());
    }
    let mut mcp_servers = configured_mcp_servers(&config, agent)?;
    let bundled_control_tools = docker
        .image_has_label(
            &spec.image,
            "io.xpressclaw.protocol",
            BUILT_IN_RUNNER_PROTOCOL,
        )
        .await;
    let github_mcp_attached = configure_bundled_github_mcp(
        &agent.runner,
        &kind,
        github.is_some(),
        bundled_control_tools,
        &mut spec.environment,
    )?;
    if bundled_control_tools
        && !agent
            .runner
            .mcp_servers
            .iter()
            .any(|name| name == "xpressclaw")
    {
        mcp_servers.push(xpressclaw_control_mcp_server(
            &agent.name,
            control_task_id.as_deref(),
            control_plane_port,
            docker.runtime(),
        ));
    }
    if let Some(access) = github.as_ref() {
        if github_mcp_attached {
            mcp_servers.push(access.mcp_server());
        } else if !bundled_control_tools {
            warn!(
                image = spec.image,
                repository = access.repository(),
                "runner image does not include the constrained GitHub MCP server"
            );
        }
    }
    let workload_id = agent.name.as_str();
    let live = processes.get_or_start(&docker, workload_id, &spec).await?;
    if let Err(error) = sessions.set_container(attempt_id, &live.container_id) {
        processes.invalidate(workload_id, &live.process).await;
        let _ = docker.stop_preserving(workload_id).await;
        return Err(error);
    }
    let running = match sessions.transition_attempt(
        attempt_id,
        "running",
        &format!("{kind} is working over ACP"),
        None,
        None,
    ) {
        Ok(running) => running,
        Err(error) => {
            processes.invalidate(workload_id, &live.process).await;
            if docker.stop_preserving(workload_id).await.is_ok() {
                let _ = sessions.clear_container(attempt_id);
            }
            return Err(error);
        }
    };
    if attempt_is_terminal(&running.status) {
        processes.invalidate(workload_id, &live.process).await;
        if docker.stop_preserving(workload_id).await.is_ok() {
            let _ = sessions.clear_container(attempt_id);
        }
        return Ok(());
    }

    let attempt = sessions.get_attempt(attempt_id)?;
    let recorder = AcpEventRecorder::new(
        db.clone(),
        attempt.session_id.clone(),
        attempt_id,
        item.task_id.clone(),
        kind.clone(),
    );
    let turn = live
        .process
        .run_turn(
            AcpTurnRuntime::new(recorder, elicitation_broker, turn_controls),
            session_start,
            Path::new(&container_workspace),
            &prompt.content,
            AcpTurnOptions {
                model: agent.runner.model.clone(),
                session_config: requested_session_config,
                mcp_servers,
                image_attachments: prompt.attachments,
            },
        )
        .await;
    if turn.is_err() {
        if processes.invalidate(workload_id, &live.process).await {
            docker.stop_preserving(workload_id).await?;
        }
    }
    sessions.clear_container(attempt_id)?;
    let turn = turn?;
    let current = sessions.get_attempt(attempt_id)?;
    if matches!(current.status.as_str(), "cancelled" | "interrupted") {
        return Ok(());
    }
    if turn.interrupted {
        sessions.add_artifact(
            attempt_id,
            "runner_output",
            "Interrupted ACP event transcript",
            Some(&turn.diagnostic),
            None,
            json!({ "protocol": "acp", "stop_reason": turn.stop_reason, "runner": kind }),
        )?;
        sessions.set_native_session(attempt_id, &turn.session_id)?;
        let interrupted = sessions.transition_attempt(
            attempt_id,
            "interrupted",
            "Agent interrupted to apply new guidance",
            None,
            None,
        )?;
        if interrupted.status != "interrupted" {
            return Ok(());
        }
        let queue = TaskQueue::new(db.clone());
        queue.complete(item.id, "interrupted to apply new guidance")?;
        let next_status = if queue.has_queued_for_task(&item.task_id)? {
            "in_progress"
        } else {
            "pending"
        };
        board.update_status(&item.task_id, next_status, Some(&item.agent_id))?;
        sessions.refresh_status(&item.agent_id)?;
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
    let completed = sessions.transition_attempt(
        attempt_id,
        "completed",
        &completion_summary,
        Some(&turn.summary),
        None,
    )?;
    if completed.status != "completed" {
        return Ok(());
    }
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

fn attempt_is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
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
            if matches!(attempt.status.as_str(), "cancelled" | "interrupted") {
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
    if let Some(agent) = crate::acp::ACP_AGENTS.iter().find(|agent| {
        backend == agent.kind
            || (agent.kind.len() >= 4 && backend.contains(agent.kind))
            || (agent.kind == "github-copilot" && backend.contains("copilot"))
    }) {
        Ok(agent.kind.to_string())
    } else if !agent.runner.command.is_empty() {
        Ok("custom".to_string())
    } else {
        Err(Error::Backend(format!(
            "session '{}' needs runner.kind or runner.command (backend was '{}')",
            agent.name, agent.backend
        )))
    }
}

struct AgentPrompt {
    content: String,
    attachments: Vec<PromptImageAttachment>,
}

fn prepend_unresumed_interrupted_prompt(
    db: &Arc<Database>,
    item: &QueueItem,
    attempt_id: &str,
    prompt: &mut AgentPrompt,
) -> Result<()> {
    let previous_prompt: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT prompt FROM work_attempts
             WHERE task_id = ?1 AND id != ?2 AND status = 'interrupted'
               AND native_session_id IS NULL AND prompt != ''
             ORDER BY COALESCE(completed_at, created_at) DESC LIMIT 1",
            rusqlite::params![item.task_id, attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    let Some(previous_prompt) = previous_prompt else {
        return Ok(());
    };
    if prompt.content.trim().is_empty() {
        prompt.content = previous_prompt;
    } else if prompt.content != previous_prompt
        && !prompt
            .content
            .starts_with(&format!("{previous_prompt}\n\n"))
    {
        prompt.content = format!(
            "{previous_prompt}\n\nAdditional guidance from the user:\n{}",
            prompt.content
        );
    }
    Ok(())
}

fn build_prompt(db: &Arc<Database>, item: &QueueItem, attempt_id: &str) -> Result<AgentPrompt> {
    let task = TaskBoard::new(db.clone()).get(&item.task_id)?;
    let previous_started: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT started_at FROM work_attempts
                 WHERE task_id = ?1 AND id != ?2 AND started_at IS NOT NULL
                   AND NOT (status = 'interrupted' AND native_session_id IS NULL)
                 ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![item.task_id, attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    let pending_user_messages = TaskConversation::new(db.clone())
        .get_user_messages_since(&item.task_id, previous_started.as_deref())?;
    if !pending_user_messages.is_empty() {
        let content = pending_user_messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let attachments = pending_user_messages
            .into_iter()
            .flat_map(|message| message.attachments)
            .collect();
        return Ok(AgentPrompt {
            content,
            attachments,
        });
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
    let content = match (description, from_project_composer) {
        (Some(description), true) => description.to_string(),
        (Some(description), false) => format!("{}\n\n{}", task.title, description),
        (None, _) => task.title,
    };
    Ok(AgentPrompt {
        content,
        attachments: Vec::new(),
    })
}

/// Pick how this task enters its ACP conversation.
///
/// Follow-ups resume the task's branch while it is still active. Reopening an
/// older task forks its saved branch, and first turns fork either their
/// dependency or the project's active branch. Agents without `session/fork`
/// support fall back to resuming the selected source inside `run_turn`.
fn session_start(db: &Arc<Database>, item: &QueueItem, runner: &str) -> Result<AcpSessionStart> {
    let board = TaskBoard::new(db.clone());
    let task = board.get(&item.task_id)?;

    let task_session: Option<(String, String)> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, native_session_id FROM work_attempts
             WHERE task_id = ?1 AND session_id = ?2 AND runner = ?3
               AND native_session_id IS NOT NULL
               AND status IN ('completed', 'interrupted')
             ORDER BY COALESCE(completed_at, created_at) DESC, rowid DESC LIMIT 1",
            rusqlite::params![item.task_id, item.agent_id, runner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Error::from)
    })?;

    let project_session: Option<(String, String)> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, native_session_id FROM work_attempts
             WHERE session_id = ?1 AND runner = ?2
               AND native_session_id IS NOT NULL
               AND status IN ('completed', 'interrupted')
             ORDER BY COALESCE(completed_at, created_at) DESC, rowid DESC LIMIT 1",
            rusqlite::params![item.agent_id, runner],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if let Some((task_attempt_id, task_session_id)) = task_session {
        return if project_session
            .as_ref()
            .is_some_and(|(project_attempt_id, _)| project_attempt_id != &task_attempt_id)
        {
            Ok(AcpSessionStart::Fork(task_session_id))
        } else {
            Ok(AcpSessionStart::Resume(task_session_id))
        };
    }

    let dependency_session: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT a.native_session_id
             FROM task_dependencies d
             JOIN work_attempts a ON a.task_id = d.depends_on_id
             WHERE d.task_id = ?1 AND a.session_id = ?2 AND a.runner = ?3
               AND a.native_session_id IS NOT NULL
               AND a.status IN ('completed', 'interrupted')
             ORDER BY COALESCE(a.completed_at, a.created_at) DESC, a.rowid DESC LIMIT 1",
            rusqlite::params![item.task_id, item.agent_id, runner],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if let Some(dependency_session) = dependency_session {
        return Ok(AcpSessionStart::Fork(dependency_session));
    }

    let start_new = task
        .context
        .as_ref()
        .and_then(|context| context.get("session_mode"))
        .and_then(Value::as_str)
        == Some("new");
    if start_new {
        return Ok(AcpSessionStart::New);
    }

    Ok(
        project_session.map_or(AcpSessionStart::New, |(_, session_id)| {
            AcpSessionStart::Fork(session_id)
        }),
    )
}

async fn runner_image_ready(
    docker: &DockerManager,
    image: &str,
    built_in_image: bool,
    agent: &AgentConfig,
) -> bool {
    runner_image_compatible(
        docker,
        image,
        built_in_image,
        built_in_image && agent.runner.container_engine == ContainerEngineAccess::Host,
    )
    .await
}

/// Apply the one compatibility contract used by readiness checks and the
/// dispatcher. Keeping this in the core worker module prevents the UI from
/// accepting a stale local image that the dispatcher will reject (or vice
/// versa), which otherwise turns Prepare runner into an unnecessary registry
/// pull.
pub async fn runner_image_compatible(
    docker: &DockerManager,
    image: &str,
    built_in_image: bool,
    host_engine_image: bool,
) -> bool {
    docker.has_image(image).await
        && (!built_in_image
            || docker
                .image_has_label(image, "io.xpressclaw.protocol", BUILT_IN_RUNNER_PROTOCOL)
                .await)
        && (!host_engine_image
            || docker
                .image_has_label(image, "io.xpressclaw.container-engine", "host")
                .await)
}

fn configured_mcp_servers(config: &Config, agent: &AgentConfig) -> Result<Vec<McpServer>> {
    agent
        .runner
        .mcp_servers
        .iter()
        .map(|name| {
            let server = config.mcp_servers.get(name).ok_or_else(|| {
                Error::Backend(format!(
                    "harness references MCP server '{name}', but it is not configured"
                ))
            })?;
            mcp_server_from_config(name, server)
        })
        .collect()
}

fn continuation_task_id(task: &Task) -> Option<&str> {
    (!task.hidden && task.task_type != "IDLE").then_some(task.id.as_str())
}

fn xpressclaw_control_mcp_server(
    agent_id: &str,
    task_id: Option<&str>,
    control_plane_port: u16,
    container_runtime: &str,
) -> McpServer {
    let host = if container_runtime == "podman" {
        "host.containers.internal"
    } else {
        "host.docker.internal"
    };
    let mut env = vec![
        EnvVariable::new(
            "XPRESSCLAW_URL",
            format!("http://{host}:{control_plane_port}"),
        ),
        EnvVariable::new("XPRESSCLAW_AGENT_ID", agent_id),
    ];
    if let Some(task_id) = task_id {
        env.push(EnvVariable::new("XPRESSCLAW_TASK_ID", task_id));
    }
    // The control MCP must move in lockstep with the control plane. Runner
    // images are cached independently and can legitimately remain on an older
    // build, so execute the source embedded in this XpressClaw binary instead
    // of the image's compatibility copy.
    McpServer::Stdio(
        McpServerStdio::new("xpressclaw", BUNDLED_CONTROL_MCP_COMMAND)
            .args(vec![
                "--input-type=module".to_string(),
                "--eval".to_string(),
                BUNDLED_CONTROL_MCP_SOURCE.to_string(),
            ])
            .env(env),
    )
}

fn is_absolute_container_path(path: &str) -> bool {
    // MCP servers run inside the Linux harness container, so their command
    // paths must not be interpreted using the desktop host's path semantics.
    path.starts_with('/')
}

fn mcp_server_from_config(name: &str, config: &McpServerConfig) -> Result<McpServer> {
    let headers = || {
        config
            .headers
            .iter()
            .map(|(name, value)| HttpHeader::new(name, value))
            .collect::<Vec<_>>()
    };
    match config.server_type.as_str() {
        "stdio" => {
            let command = config
                .command
                .as_deref()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .ok_or_else(|| Error::Backend(format!("MCP server '{name}' has no command")))?;
            if !is_absolute_container_path(command) {
                return Err(Error::Backend(format!(
                    "MCP server '{name}' command must be an absolute path inside the harness container"
                )));
            }
            let env = config
                .env
                .iter()
                .map(|(name, value)| EnvVariable::new(name, value))
                .collect();
            Ok(McpServer::Stdio(
                McpServerStdio::new(name, command)
                    .args(config.args.clone())
                    .env(env),
            ))
        }
        "http" => {
            let url = config
                .url
                .as_deref()
                .ok_or_else(|| Error::Backend(format!("HTTP MCP server '{name}' has no URL")))?;
            Ok(McpServer::Http(
                McpServerHttp::new(name, url).headers(headers()),
            ))
        }
        "sse" => {
            let url = config
                .url
                .as_deref()
                .ok_or_else(|| Error::Backend(format!("SSE MCP server '{name}' has no URL")))?;
            Ok(McpServer::Sse(
                McpServerSse::new(name, url).headers(headers()),
            ))
        }
        other => Err(Error::Backend(format!(
            "MCP server '{name}' has unsupported transport '{other}'"
        ))),
    }
}

/// Merge harness defaults, workflow/task overrides, and the controls chosen
/// alongside the latest user message. Values stay keyed by opaque ACP option
/// IDs; the adapter remains the source of truth for what each option means.
fn requested_session_config(
    db: &Arc<Database>,
    agent: &AgentConfig,
    task_id: &str,
) -> Result<std::collections::HashMap<String, Value>> {
    let mut requested = agent.runner.session_config.clone();
    let task = TaskBoard::new(db.clone()).get(task_id)?;
    if let Some(overrides) = task
        .context
        .as_ref()
        .and_then(|context| context.get("session_config"))
        .and_then(Value::as_object)
    {
        requested.extend(
            overrides
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }

    let latest_message_payload: Option<String> = db.with_conn(|conn| {
        conn.query_row(
            "SELECT payload FROM session_events
             WHERE task_id = ?1 AND event_type = 'task_message_received'
             ORDER BY id DESC LIMIT 1",
            [task_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Error::from)
    })?;
    if let Some(overrides) = latest_message_payload
        .as_deref()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .as_ref()
        .and_then(|payload| payload.get("config_options"))
        .and_then(Value::as_object)
    {
        requested.extend(
            overrides
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
    Ok(requested)
}

fn build_spec(
    config: &Config,
    agent: &AgentConfig,
    kind: &str,
    docker: &DockerManager,
    github: Option<&github::GithubSessionAccess>,
) -> Result<ContainerSpec> {
    let image = resolved_runner_image(&agent.runner, kind)?;
    let workspace = resolved_workspace(config, agent);
    let container_workspace = container_workspace_path(&workspace, agent.runner.container_engine);
    let command = acp_command_for(&agent.runner, kind, &container_workspace)?;
    let command = with_startup_commands(command, &agent.runner.startup_commands);
    let mut volumes = vec![VolumeMount {
        source: workspace.display().to_string(),
        target: container_workspace.clone(),
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
    let mut environment = vec![
        "HOME=/home/node".to_string(),
        "CI=1".to_string(),
        "NO_COLOR=1".to_string(),
    ];
    let mut runner_environment = agent.runner.environment.iter().collect::<Vec<_>>();
    runner_environment.sort_by(|left, right| left.0.cmp(right.0));
    for (name, value) in runner_environment {
        if name.trim().is_empty() || name.contains('=') {
            return Err(Error::Backend(format!(
                "invalid harness environment variable name: {name:?}"
            )));
        }
        environment.push(format!("{name}={value}"));
    }
    github::extend_git_environment(&mut environment, github);
    if agent.runner.container_engine == ContainerEngineAccess::Host {
        let socket = docker.host_engine_socket().ok_or_else(|| {
            Error::DockerNotAvailable(
                "host container-engine access requires a local Docker-compatible Unix socket"
                    .to_string(),
            )
        })?;
        volumes.push(VolumeMount {
            source: socket.display().to_string(),
            target: "/var/run/docker.sock".to_string(),
            read_only: false,
        });
        environment.push("DOCKER_HOST=unix:///var/run/docker.sock".to_string());
    }

    Ok(ContainerSpec {
        image,
        memory_limit: Some(4 * 1024 * 1024 * 1024),
        cpu_limit: None,
        environment,
        volumes,
        network_mode: Some("bridge".to_string()),
        expose_port: None,
        cmd: Some(command),
        working_dir: Some(container_workspace),
        run_as_host_user: true,
    })
}

fn configure_bundled_github_mcp(
    runner: &NativeRunnerConfig,
    kind: &str,
    github_available: bool,
    bundled_control_tools: bool,
    environment: &mut Vec<String>,
) -> Result<bool> {
    let attached = github_available
        && bundled_control_tools
        && !runner.mcp_servers.iter().any(|name| name == "github");
    if attached && kind == "codex" {
        const PREFIX: &str = "CODEX_CONFIG=";
        let existing_index = environment
            .iter()
            .position(|variable| variable.starts_with(PREFIX));
        let mut codex_environment = std::collections::HashMap::new();
        if let Some(index) = existing_index {
            codex_environment.insert(
                "CODEX_CONFIG".to_string(),
                environment[index][PREFIX.len()..].to_string(),
            );
        }
        github::add_codex_mcp_guidance(&mut codex_environment)?;
        let config = codex_environment
            .remove("CODEX_CONFIG")
            .ok_or_else(|| Error::Backend("GitHub guidance did not produce CODEX_CONFIG".into()))?;
        let variable = format!("{PREFIX}{config}");
        if let Some(index) = existing_index {
            environment[index] = variable;
        } else {
            environment.push(variable);
        }
    }
    Ok(attached)
}

fn with_startup_commands(command: Vec<String>, startup_commands: &[String]) -> Vec<String> {
    if startup_commands.is_empty() {
        return command;
    }
    let startup_commands = startup_commands
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .collect::<Vec<_>>();
    let mut initializer = Sha256::new();
    for startup_command in &startup_commands {
        initializer.update(startup_command.len().to_le_bytes());
        initializer.update(startup_command.as_bytes());
    }
    let marker = format!("/tmp/.xpressclaw-environment-{:x}", initializer.finalize());
    let mut script = format!("set -eu\nmarker={marker}\nif [ ! -e \"$marker\" ]; then\n");
    for startup_command in startup_commands {
        script.push_str("  ");
        script.push_str(startup_command);
        script.push('\n');
    }
    script.push_str("  touch \"$marker\"\nfi\nexec \"$@\"");
    let mut wrapped = vec![
        "/bin/sh".to_string(),
        "-lc".to_string(),
        script,
        "xpressclaw-startup".to_string(),
    ];
    wrapped.extend(command);
    wrapped
}

fn container_workspace_path(workspace: &Path, container_engine: ContainerEngineAccess) -> String {
    if container_engine == ContainerEngineAccess::Host && cfg!(unix) {
        workspace.display().to_string()
    } else {
        "/workspace".to_string()
    }
}

fn resolved_workspace(config: &Config, agent: &AgentConfig) -> PathBuf {
    let workspace = agent
        .runner
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(expand_home)
        .map(PathBuf::from)
        .unwrap_or_else(|| config.system.workspace_dir.clone());
    canonical_or_original(&workspace)
}

pub fn resolved_runner_image(config: &NativeRunnerConfig, kind: &str) -> Result<String> {
    let desired_default = default_native_runner_image(kind, config.container_engine);
    let alternate_mode = match config.container_engine {
        ContainerEngineAccess::None => ContainerEngineAccess::Host,
        ContainerEngineAccess::Host => ContainerEngineAccess::None,
    };
    let alternate_default = default_native_runner_image(kind, alternate_mode);
    let configured_image = config.image.trim();
    let built_in_image = [desired_default, alternate_default]
        .into_iter()
        .flatten()
        .any(|image| {
            configured_image == image || local_runner_image_alias(image) == Some(configured_image)
        });
    if configured_image.is_empty() || built_in_image {
        return desired_default.map(str::to_owned).ok_or_else(|| {
            Error::Backend(format!(
                "runner '{kind}' requires an explicit container image"
            ))
        });
    }
    Ok(configured_image.to_string())
}

/// Local tags used by the ACP runner images. A published image is the
/// default, but retaining these aliases lets existing developer builds run
/// without a forced retag or registry pull.
pub fn local_runner_image_alias(image: &str) -> Option<&'static str> {
    local_runner_image(image)
}

fn acp_command_for(
    config: &NativeRunnerConfig,
    kind: &str,
    container_workspace: &str,
) -> Result<Vec<String>> {
    if !config.command.is_empty() {
        return Ok(config
            .command
            .iter()
            .map(|part| part.replace("{workspace}", container_workspace))
            .collect());
    }
    agent_definition(kind)
        .map(|agent| {
            agent
                .command
                .iter()
                .map(|part| (*part).to_string())
                .collect()
        })
        .ok_or_else(|| {
            Error::Backend(format!(
                "ACP runner '{kind}' requires an explicit server command"
            ))
        })
}

fn auth_candidates(kind: &str) -> Vec<(PathBuf, &'static str, bool)> {
    let Some(home) = host_home() else {
        return Vec::new();
    };
    agent_definition(kind)
        .map(|agent| {
            agent
                .auth_mounts
                .iter()
                .map(|mount| (home.join(mount.source), mount.target, mount.read_only))
                .collect()
        })
        .unwrap_or_default()
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
    auth_candidates(kind)
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
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim(resolved)
}

/// Strip the `\\?\` prefix Windows `canonicalize()` adds to drive paths;
/// Docker Desktop's bind-mount parser rejects it. Others pass through as-is.
#[cfg(windows)]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(not(windows))]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
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
    fn mcp_commands_use_container_path_semantics() {
        assert!(is_absolute_container_path("/opt/project/mcp-server"));
        assert!(!is_absolute_container_path("npx"));
        assert!(!is_absolute_container_path(r"C:\tools\mcp-server.exe"));
    }

    #[test]
    fn converts_selected_mcp_servers_to_acp_session_configuration() {
        let stdio = mcp_server_from_config(
            "project-tools",
            &McpServerConfig {
                command: Some("/opt/project/mcp-server".into()),
                args: vec!["--stdio".into()],
                env: [("PROJECT_ROOT".into(), "/workspace".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
        let McpServer::Stdio(stdio) = stdio else {
            panic!("expected stdio MCP configuration");
        };
        assert_eq!(stdio.name, "project-tools");
        assert_eq!(stdio.command, PathBuf::from("/opt/project/mcp-server"));
        assert_eq!(stdio.args, ["--stdio"]);
        assert_eq!(stdio.env.len(), 1);

        let http = mcp_server_from_config(
            "metrics",
            &McpServerConfig {
                server_type: "http".into(),
                url: Some("https://mcp.example.test/rpc".into()),
                headers: [("Authorization".into(), "Bearer test".into())]
                    .into_iter()
                    .collect(),
                ..Default::default()
            },
        )
        .unwrap();
        let McpServer::Http(http) = http else {
            panic!("expected HTTP MCP configuration");
        };
        assert_eq!(http.name, "metrics");
        assert_eq!(http.url, "https://mcp.example.test/rpc");
        assert_eq!(http.headers.len(), 1);

        let error = mcp_server_from_config(
            "host-only",
            &McpServerConfig {
                command: Some("npx".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("absolute path inside the harness container"));
    }

    #[test]
    fn scopes_the_bundled_control_mcp_to_the_current_project() {
        let server = xpressclaw_control_mcp_server("dgx-codex", Some("task-123"), 9123, "docker");
        let McpServer::Stdio(server) = server else {
            panic!("expected stdio MCP configuration");
        };

        assert_eq!(server.name, "xpressclaw");
        assert_eq!(server.command, PathBuf::from(BUNDLED_CONTROL_MCP_COMMAND));
        assert_eq!(
            &server.args[..2],
            ["--input-type=module".to_string(), "--eval".to_string()]
        );
        assert!(server.args[2].contains("continuation_task_id"));
        assert!(server.args[2].ends_with("await main();\n"));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_URL"
                && variable.value == "http://host.docker.internal:9123"
        }));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_AGENT_ID" && variable.value == "dgx-codex"
        }));
        assert!(server.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_TASK_ID" && variable.value == "task-123"
        }));

        let McpServer::Stdio(podman) =
            xpressclaw_control_mcp_server("dgx-codex", Some("task-123"), 9123, "podman")
        else {
            panic!("expected stdio MCP configuration");
        };
        assert!(podman.env.iter().any(|variable| {
            variable.name == "XPRESSCLAW_URL"
                && variable.value == "http://host.containers.internal:9123"
        }));

        let McpServer::Stdio(idle) =
            xpressclaw_control_mcp_server("dgx-codex", None, 9123, "docker")
        else {
            panic!("expected stdio MCP configuration");
        };
        assert!(!idle
            .env
            .iter()
            .any(|variable| variable.name == "XPRESSCLAW_TASK_ID"));
    }

    #[test]
    fn hidden_idle_tasks_do_not_bind_scheduled_wakeups() {
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        let board = TaskBoard::new(db);
        let visible = board
            .create(&CreateTask {
                title: "Visible work".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let idle = board
            .create_idle_task("atlas", "Look for proactive work")
            .unwrap();

        assert_eq!(continuation_task_id(&visible), Some(visible.id.as_str()));
        assert_eq!(continuation_task_id(&idle), None);
    }

    #[test]
    fn message_controls_override_workflow_and_harness_session_defaults() {
        use crate::sessions::NewEvent;
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        let sessions = SessionManager::new(db.clone());
        sessions.ensure("atlas", Some("Atlas")).unwrap();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Configurable turn".into(),
                agent_id: Some("atlas".into()),
                context: Some(json!({
                    "session_config": {
                        "mode": "plan",
                        "thought_level": "medium"
                    }
                })),
                ..Default::default()
            })
            .unwrap();
        sessions
            .append_event(
                "atlas",
                NewEvent {
                    attempt_id: None,
                    task_id: Some(&task.id),
                    source_type: "user",
                    source_id: None,
                    event_type: "task_message_received",
                    summary: "Change controls",
                    payload: json!({
                        "config_options": {
                            "mode": "build",
                            "approval_policy": true
                        }
                    }),
                },
            )
            .unwrap();

        let agent = AgentConfig {
            runner: NativeRunnerConfig {
                session_config: [
                    ("mode".into(), json!("default")),
                    ("model".into(), json!("fast")),
                    ("approval_policy".into(), json!(false)),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        let requested = requested_session_config(&db, &agent, &task.id).unwrap();
        assert_eq!(requested.get("model"), Some(&json!("fast")));
        assert_eq!(requested.get("thought_level"), Some(&json!("medium")));
        assert_eq!(requested.get("mode"), Some(&json!("build")));
        assert_eq!(requested.get("approval_policy"), Some(&json!(true)));
    }

    #[test]
    fn resolves_legacy_claude_backend_to_native_cli() {
        let agent = AgentConfig {
            backend: "claude-sdk".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_runner_kind(&agent).unwrap(), "claude");
    }

    #[test]
    fn does_not_confuse_copilot_with_pi() {
        let agent = AgentConfig {
            backend: "copilot-cli".to_string(),
            ..Default::default()
        };
        assert_eq!(resolve_runner_kind(&agent).unwrap(), "github-copilot");
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
            acp_command_for(&config, "custom", "/workspace").unwrap(),
            vec!["runner", "--cwd=/workspace"]
        );
    }

    #[test]
    fn wraps_acp_command_with_one_time_project_environment_startup() {
        let wrapped = with_startup_commands(
            vec!["qwen".into(), "--acp".into()],
            &["npm ci".into(), "docker compose up -d".into()],
        );
        assert_eq!(&wrapped[..2], ["/bin/sh", "-lc"]);
        assert!(wrapped[2].contains("/tmp/.xpressclaw-environment-"));
        assert!(wrapped[2].contains("  npm ci\n  docker compose up -d"));
        assert!(wrapped[2].contains("touch \"$marker\""));
        assert!(wrapped[2].ends_with("exec \"$@\""));
        assert_eq!(&wrapped[4..], ["qwen", "--acp"]);
    }

    #[test]
    fn starts_the_builtin_acp_servers() {
        let config = NativeRunnerConfig::default();
        assert_eq!(
            acp_command_for(&config, "codex", "/workspace").unwrap(),
            vec!["codex-acp"]
        );
        assert_eq!(
            acp_command_for(&config, "claude", "/workspace").unwrap(),
            vec!["claude-agent-acp"]
        );
        assert_eq!(
            acp_command_for(&config, "opencode", "/workspace").unwrap(),
            vec!["opencode", "acp"]
        );
    }

    #[test]
    fn codex_runner_gets_github_guidance_only_when_bundled_mcp_is_attached() {
        let runner = NativeRunnerConfig::default();
        let mut environment = vec!["HOME=/home/node".to_string()];

        assert!(
            configure_bundled_github_mcp(&runner, "codex", true, true, &mut environment).unwrap()
        );
        let config: Value = serde_json::from_str(
            environment
                .iter()
                .find_map(|variable| variable.strip_prefix("CODEX_CONFIG="))
                .unwrap(),
        )
        .unwrap();
        assert!(config["developer_instructions"]
            .as_str()
            .unwrap()
            .contains("GitHub runtime"));

        let mut custom_image_environment = vec![
            "HOME=/home/node".to_string(),
            r#"CODEX_CONFIG={"developer_instructions":"Use the image's shell gh."}"#.to_string(),
        ];
        let original_custom_environment = custom_image_environment.clone();
        assert!(!configure_bundled_github_mcp(
            &runner,
            "codex",
            true,
            false,
            &mut custom_image_environment
        )
        .unwrap());
        assert_eq!(custom_image_environment, original_custom_environment);

        let mut missing_access_environment = Vec::new();
        assert!(!configure_bundled_github_mcp(
            &runner,
            "codex",
            false,
            true,
            &mut missing_access_environment
        )
        .unwrap());

        let configured_runner = NativeRunnerConfig {
            mcp_servers: vec!["github".to_string()],
            ..Default::default()
        };
        let mut configured_environment = Vec::new();
        assert!(!configure_bundled_github_mcp(
            &configured_runner,
            "codex",
            true,
            true,
            &mut configured_environment
        )
        .unwrap());
    }

    #[test]
    fn startup_interrupt_preserves_original_messages_and_images() {
        use crate::tasks::attachments::DecodedImageAttachment;
        use crate::tasks::board::CreateTask;

        let db = Arc::new(Database::open_memory().unwrap());
        let board = TaskBoard::new(db.clone());
        SessionManager::new(db.clone())
            .ensure("atlas", Some("atlas"))
            .unwrap();
        let task = board
            .create(&CreateTask {
                title: "Inspect the screenshot".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        TaskConversation::new(db.clone())
            .add_message_with_attachments(
                &task.id,
                "user",
                "Original request",
                &[DecodedImageAttachment {
                    name: "screen.png".into(),
                    mime_type: "image/png".into(),
                    data: b"original image".to_vec(),
                }],
            )
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        queue.enqueue(&task.id, "atlas").unwrap();
        let first = queue.claim("atlas").unwrap().unwrap();
        let first_attempt_id = first.attempt_id.as_deref().unwrap();
        let first_prompt = build_prompt(&db, &first, first_attempt_id).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET prompt = ?1 WHERE id = ?2",
                rusqlite::params![first_prompt.content, first_attempt_id],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                first_attempt_id,
                "interrupted",
                "Stopped during startup",
                None,
                None,
            )
            .unwrap();
        queue.complete(first.id, "interrupted").unwrap();
        TaskConversation::new(db.clone())
            .add_message(&task.id, "user", "New guidance")
            .unwrap();
        let continuation = queue
            .enqueue_continuation(&task.id, "atlas")
            .unwrap()
            .unwrap();
        let continuation_attempt_id = continuation.attempt_id.as_deref().unwrap();

        let mut prompt = build_prompt(&db, &continuation, continuation_attempt_id).unwrap();
        prepend_unresumed_interrupted_prompt(
            &db,
            &continuation,
            continuation_attempt_id,
            &mut prompt,
        )
        .unwrap();

        assert_eq!(prompt.content, "Original request\n\nNew guidance");
        assert_eq!(prompt.attachments.len(), 1);
        assert_eq!(prompt.attachments[0].name, "screen.png");
        assert_eq!(prompt.attachments[0].data, b"original image");
    }

    #[test]
    fn selects_fork_resume_and_fresh_conversation_contexts() {
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
                    completed_at = '2026-01-01 00:00:01' WHERE id = ?1",
                [first_attempt],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .set_native_session(first_attempt, "thread-1")
            .unwrap();

        let failed = board
            .create(&CreateTask {
                title: "Failed setup".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        let failed_item = queue.enqueue(&failed.id, "atlas").unwrap();
        let failed_attempt = failed_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex' WHERE id = ?1",
                [failed_attempt],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            .set_native_session(failed_attempt, "failed-thread")
            .unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                failed_attempt,
                "failed",
                "Setup failed",
                None,
                Some("bad config"),
            )
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
            session_start(&db, &regular_item, "codex").unwrap(),
            AcpSessionStart::Fork("thread-1".into())
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
        assert_eq!(
            session_start(&db, &fresh_item, "codex").unwrap(),
            AcpSessionStart::New
        );

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
            session_start(&db, &dependent_item, "codex").unwrap(),
            AcpSessionStart::Fork("thread-1".into())
        );

        let regular_attempt = regular_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex', status = 'completed',
                    completed_at = '2026-01-01 00:00:01' WHERE id = ?1",
                [regular_attempt],
            )
        })
        .unwrap();
        SessionManager::new(db.clone())
            // Legacy attempts can share a mutable ACP session ID. Attempt
            // identity still distinguishes the current task from an old one.
            .set_native_session(regular_attempt, "thread-1")
            .unwrap();

        let active_follow_up = queue.enqueue(&regular.id, "atlas").unwrap();
        assert_eq!(
            session_start(&db, &active_follow_up, "codex").unwrap(),
            AcpSessionStart::Resume("thread-1".into())
        );

        let old_task_follow_up = queue.enqueue(&first.id, "atlas").unwrap();
        assert_eq!(
            session_start(&db, &old_task_follow_up, "codex").unwrap(),
            AcpSessionStart::Fork("thread-1".into())
        );
    }

    #[test]
    fn scheduled_wakeup_resumes_the_armed_tasks_codex_conversation() {
        use crate::tasks::board::CreateTask;
        use crate::tasks::scheduler::{CreateOneShotSchedule, ScheduleManager};

        let db = Arc::new(Database::open_memory().unwrap());
        let board = TaskBoard::new(db.clone());
        let sessions = SessionManager::new(db.clone());
        sessions.ensure("dgx-codex", Some("DGX")).unwrap();
        let original = board
            .create(&CreateTask {
                title: "Run the DGX experiment".into(),
                agent_id: Some("dgx-codex".into()),
                ..Default::default()
            })
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        let original_item = queue.enqueue(&original.id, "dgx-codex").unwrap();
        let original_attempt = original_item.attempt_id.as_deref().unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET runner = 'codex' WHERE id = ?1",
                [original_attempt],
            )
        })
        .unwrap();
        sessions
            .set_native_session(original_attempt, "codex-thread-1")
            .unwrap();
        sessions
            .transition_attempt(
                original_attempt,
                "completed",
                "Waiting",
                Some("Waiting"),
                None,
            )
            .unwrap();
        queue.complete(original_item.id, "Waiting").unwrap();

        let schedules = ScheduleManager::new(db.clone());
        let wakeup = schedules
            .create_one_shot(&CreateOneShotSchedule {
                name: "Check DGX".into(),
                run_at: None,
                delay_seconds: Some(5 * 60 * 60),
                agent_id: "dgx-codex".into(),
                title: "Resume the DGX experiment".into(),
                description: Some("Inspect the results and continue the active goal.".into()),
                continuation_task_id: Some(original.id.clone()),
            })
            .unwrap();
        let wakeup_task = schedules.trigger(&wakeup.id, &board).unwrap();
        let wakeup_item = queue
            .list(Some("dgx-codex"), Some("queued"), 10)
            .unwrap()
            .into_iter()
            .find(|item| item.task_id == wakeup_task.id)
            .unwrap();

        assert_eq!(wakeup_task.id, original.id);
        assert_eq!(
            session_start(&db, &wakeup_item, "codex").unwrap(),
            AcpSessionStart::Resume("codex-thread-1".into())
        );
        let messages = TaskConversation::new(db)
            .get_messages(&original.id)
            .unwrap();
        assert!(messages.iter().any(|message| {
            message
                .content
                .contains("Inspect the results and continue the active goal.")
        }));
    }

    #[test]
    fn selects_a_minimal_image_for_each_native_runner() {
        assert_eq!(
            default_native_runner_image("codex", ContainerEngineAccess::None),
            Some("ghcr.io/xpressai/xpressclaw-runner-codex:latest")
        );
        assert_eq!(
            default_native_runner_image("claude", ContainerEngineAccess::None),
            Some("ghcr.io/xpressai/xpressclaw-runner-claude:latest")
        );
        assert_eq!(
            default_native_runner_image("opencode", ContainerEngineAccess::None),
            Some("ghcr.io/xpressai/xpressclaw-runner-opencode:latest")
        );
        assert_eq!(
            default_native_runner_image("custom", ContainerEngineAccess::None),
            None
        );
    }

    #[test]
    fn host_engine_mode_selects_a_docker_cli_image_and_same_path_workspace() {
        let config = NativeRunnerConfig {
            kind: "codex".into(),
            image: "ghcr.io/xpressai/xpressclaw-runner-codex:latest".into(),
            container_engine: ContainerEngineAccess::Host,
            ..Default::default()
        };
        assert_eq!(
            resolved_runner_image(&config, "codex").unwrap(),
            "ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"
        );
        let workspace = Path::new("/home/me/project");
        assert_eq!(
            container_workspace_path(workspace, ContainerEngineAccess::Host),
            if cfg!(unix) {
                "/home/me/project"
            } else {
                "/workspace"
            }
        );
    }

    #[test]
    fn host_engine_mode_migrates_a_local_minimal_alias() {
        let config = NativeRunnerConfig {
            kind: "codex".into(),
            image: "xpressclaw-runner-codex:latest".into(),
            container_engine: ContainerEngineAccess::Host,
            ..Default::default()
        };
        assert_eq!(
            resolved_runner_image(&config, "codex").unwrap(),
            "ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"
        );
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
        assert_eq!(
            local_runner_image_alias("ghcr.io/xpressai/xpressclaw-runner-codex-docker:latest"),
            Some("xpressclaw-runner-codex-docker:latest")
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
