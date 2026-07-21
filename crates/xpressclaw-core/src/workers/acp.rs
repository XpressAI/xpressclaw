//! Agent Client Protocol transport and event normalization for isolated runners.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub use agent_client_protocol::schema::v1::CreateElicitationResponse;
use agent_client_protocol::schema::v1::{
    BooleanConfigOptionCapabilities, ClientCapabilities, ClientSessionCapabilities, ContentBlock,
    CreateElicitationRequest, ElicitationAction, ElicitationCapabilities,
    ElicitationFormCapabilities, InitializeRequest, LoadSessionRequest, McpServer,
    NewSessionRequest, PermissionOptionKind, PlanEntryStatus, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigOptionValue, SessionConfigOptionsCapabilities,
    SessionConfigSelectOptions, SessionModeState, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionModeRequest, TextContent, ToolCallStatus,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use bollard::container::LogOutput;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use uuid::Uuid;

use crate::db::Database;
use crate::docker::manager::AttachedContainer;
use crate::error::{Error, Result};
use crate::sessions::{NewEvent, SessionManager};
use crate::tasks::board::{ReportedSubtask, TaskBoard, TaskStatus};

const MAX_EVENTS: usize = 250;
const MAX_TRANSCRIPT_UPDATES: usize = 500;
const MAX_DIAGNOSTIC_BYTES: usize = 200_000;

struct PendingElicitation {
    task_id: String,
    attempt_id: String,
    responder: oneshot::Sender<CreateElicitationResponse>,
}

/// Coordinates agent-initiated forms with the task HTTP API while the ACP
/// prompt remains in flight. The durable session event owns presentation and
/// recovery; this broker only owns the live response channel to the container.
#[derive(Default)]
pub struct AcpElicitationBroker {
    pending: Mutex<HashMap<String, PendingElicitation>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpElicitationResponseError {
    NotFound,
    WrongTask,
    Closed,
}

impl AcpElicitationBroker {
    pub fn new() -> Self {
        Self::default()
    }

    fn begin(
        &self,
        task_id: &str,
        attempt_id: &str,
    ) -> (String, oneshot::Receiver<CreateElicitationResponse>) {
        let id = Uuid::new_v4().to_string();
        let (responder, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(
            id.clone(),
            PendingElicitation {
                task_id: task_id.to_string(),
                attempt_id: attempt_id.to_string(),
                responder,
            },
        );
        (id, receiver)
    }

    fn abandon(&self, elicitation_id: &str) {
        self.pending.lock().unwrap().remove(elicitation_id);
    }

    pub fn respond(
        &self,
        task_id: &str,
        elicitation_id: &str,
        response: CreateElicitationResponse,
    ) -> std::result::Result<(), AcpElicitationResponseError> {
        let pending = {
            let mut entries = self.pending.lock().unwrap();
            let Some(entry) = entries.get(elicitation_id) else {
                return Err(AcpElicitationResponseError::NotFound);
            };
            if entry.task_id != task_id {
                return Err(AcpElicitationResponseError::WrongTask);
            }
            entries.remove(elicitation_id).unwrap()
        };
        pending
            .responder
            .send(response)
            .map_err(|_| AcpElicitationResponseError::Closed)
    }

    /// Cancel live questions for an attempt before its container is stopped.
    /// Returning an ACP cancellation releases any adapter waiting inside a
    /// tool call instead of leaving the dispatcher future parked forever.
    pub fn cancel_attempt(&self, attempt_id: &str) -> usize {
        self.cancel_matching(|entry| entry.attempt_id == attempt_id)
    }

    pub fn cancel_task(&self, task_id: &str) -> usize {
        self.cancel_matching(|entry| entry.task_id == task_id)
    }

    fn cancel_matching(&self, matches: impl Fn(&PendingElicitation) -> bool) -> usize {
        let pending = {
            let mut entries = self.pending.lock().unwrap();
            let ids = entries
                .iter()
                .filter(|(_, entry)| matches(entry))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| entries.remove(&id))
                .collect::<Vec<_>>()
        };
        let count = pending.len();
        for entry in pending {
            let _ = entry
                .responder
                .send(CreateElicitationResponse::new(ElicitationAction::Cancel));
        }
        count
    }
}

/// Result of one ACP prompt turn. ACP session IDs remain opaque and are only
/// used with `session/resume` or `session/load` on later attempts.
#[derive(Debug)]
pub struct AcpTurnResult {
    pub session_id: String,
    pub summary: String,
    pub stop_reason: String,
    pub diagnostic: String,
}

/// Per-turn client choices applied after the ACP session is created or
/// resumed and before its prompt is sent.
#[derive(Debug, Default)]
pub struct AcpTurnOptions {
    pub model: Option<String>,
    pub session_config: HashMap<String, Value>,
    pub mcp_servers: Vec<McpServer>,
}

#[derive(Debug, Default)]
struct TurnState {
    assistant_text: String,
    last_assistant_text: String,
    current_message_id: Option<String>,
    pending_thought: String,
    tool_titles: HashMap<String, String>,
    transcript: Vec<Value>,
    capture_prompt_output: bool,
}

/// Persists standardized ACP updates as the semantic activity shown on task
/// pages. The recorder deliberately knows nothing about Codex, Claude, or any
/// other provider-specific event schema.
#[derive(Clone)]
pub struct AcpEventRecorder {
    db: Arc<Database>,
    logical_session_id: String,
    attempt_id: String,
    task_id: String,
    runner: String,
    emitted: Arc<AtomicUsize>,
    state: Arc<Mutex<TurnState>>,
}

impl AcpEventRecorder {
    pub fn new(
        db: Arc<Database>,
        logical_session_id: impl Into<String>,
        attempt_id: impl Into<String>,
        task_id: impl Into<String>,
        runner: impl Into<String>,
    ) -> Self {
        Self {
            db,
            logical_session_id: logical_session_id.into(),
            attempt_id: attempt_id.into(),
            task_id: task_id.into(),
            runner: runner.into(),
            emitted: Arc::new(AtomicUsize::new(0)),
            state: Arc::new(Mutex::new(TurnState::default())),
        }
    }

    fn reset_prompt_output(&self) {
        let mut state = self.state.lock().unwrap();
        state.assistant_text.clear();
        state.last_assistant_text.clear();
        state.current_message_id = None;
        state.pending_thought.clear();
        state.capture_prompt_output = true;
    }

    fn record_notification(&self, notification: SessionNotification) -> Result<()> {
        let payload = serde_json::to_value(&notification.update)
            .map_err(|error| Error::Backend(format!("failed to serialize ACP update: {error}")))?;
        {
            let mut state = self.state.lock().unwrap();
            if state.transcript.len() < MAX_TRANSCRIPT_UPDATES {
                state.transcript.push(payload.clone());
            }
        }

        match notification.update {
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let ContentBlock::Text(text) = chunk.content {
                    let message_id = chunk.message_id.map(|id| id.to_string());
                    let starts_new_message = {
                        let state = self.state.lock().unwrap();
                        if !state.capture_prompt_output {
                            return Ok(());
                        }
                        message_id.is_some()
                            && state.current_message_id.is_some()
                            && message_id != state.current_message_id
                    };
                    if starts_new_message {
                        self.flush_agent_message()?;
                    }
                    self.flush_thought()?;
                    let mut state = self.state.lock().unwrap();
                    if message_id.is_some() {
                        state.current_message_id = message_id;
                    }
                    state.assistant_text.push_str(&text.text);
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let ContentBlock::Text(text) = chunk.content {
                    let capture = self.state.lock().unwrap().capture_prompt_output;
                    if capture {
                        self.flush_agent_message()?;
                        self.state
                            .lock()
                            .unwrap()
                            .pending_thought
                            .push_str(&text.text);
                    }
                }
            }
            SessionUpdate::ToolCall(call) => {
                self.flush_prompt_output()?;
                self.state
                    .lock()
                    .unwrap()
                    .tool_titles
                    .insert(call.tool_call_id.to_string(), call.title.clone());
                let summary = tool_summary(&call.title, call.status);
                self.append_event("tool_call", &summary, payload)?;
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.flush_prompt_output()?;
                let id = update.tool_call_id.to_string();
                let title = {
                    let mut state = self.state.lock().unwrap();
                    if let Some(title) = update.fields.title.as_ref() {
                        state.tool_titles.insert(id.clone(), title.clone());
                    }
                    state
                        .tool_titles
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| "Tool call".to_string())
                };
                if let Some(status) = update.fields.status {
                    let summary = tool_summary(&title, status);
                    self.append_event("tool_call_update", &summary, payload)?;
                }
            }
            SessionUpdate::Plan(plan) => {
                self.flush_prompt_output()?;
                let reported: Vec<ReportedSubtask> = plan
                    .entries
                    .iter()
                    .map(|entry| ReportedSubtask {
                        title: entry.content.clone(),
                        status: match entry.status {
                            PlanEntryStatus::Pending => TaskStatus::Pending,
                            PlanEntryStatus::InProgress => TaskStatus::InProgress,
                            PlanEntryStatus::Completed => TaskStatus::Completed,
                            _ => TaskStatus::Pending,
                        },
                    })
                    .collect();
                TaskBoard::new(self.db.clone()).sync_reported_subtasks(
                    &self.task_id,
                    &self.attempt_id,
                    &reported,
                )?;
                let completed = reported
                    .iter()
                    .filter(|entry| entry.status == TaskStatus::Completed)
                    .count();
                self.append_event(
                    "plan",
                    &format!("Updated plan: {completed}/{} complete", reported.len()),
                    payload,
                )?;
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                self.flush_prompt_output()?;
                self.append_event(
                    "session_mode",
                    &format!("Switched to {} mode", update.current_mode_id),
                    payload,
                )?;
            }
            SessionUpdate::SessionInfoUpdate(update) => {
                self.flush_prompt_output()?;
                if let Some(title) = update.title.take() {
                    self.append_event("session_info", &format!("Session title: {title}"), payload)?;
                }
            }
            SessionUpdate::UsageUpdate(_) => {
                self.flush_prompt_output()?;
                self.append_event("usage", "Updated context usage", payload)?;
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.flush_prompt_output()?;
                self.record_session_controls(&update.config_options, None)?;
            }
            SessionUpdate::AvailableCommandsUpdate(update) => {
                self.flush_prompt_output()?;
                self.append_event(
                    "available_commands",
                    &format!(
                        "Agent advertised {} commands",
                        update.available_commands.len()
                    ),
                    json!({ "available_commands": update.available_commands }),
                )?;
            }
            SessionUpdate::UserMessageChunk(_) => {}
            _ => {}
        }
        Ok(())
    }

    fn record_permission(&self, request: &RequestPermissionRequest, choice: Option<&str>) {
        let _ = self.flush_prompt_output();
        let title = request
            .tool_call
            .fields
            .title
            .as_deref()
            .unwrap_or("tool call");
        let summary = choice.map_or_else(
            || format!("Could not approve {title}"),
            |choice| format!("Approved {title}: {choice}"),
        );
        let payload = serde_json::to_value(request).unwrap_or_else(|_| json!({}));
        let _ = self.append_event("permission", &summary, payload);
    }

    fn record_elicitation_pending(
        &self,
        elicitation_id: &str,
        request: &CreateElicitationRequest,
    ) -> Result<()> {
        self.flush_prompt_output()?;
        SessionManager::new(self.db.clone()).transition_attempt(
            &self.attempt_id,
            "waiting_for_input",
            "Waiting for your answer",
            None,
            None,
        )?;
        TaskBoard::new(self.db.clone()).update_status(
            &self.task_id,
            "waiting_for_input",
            Some(&self.logical_session_id),
        )?;
        let mut payload = serde_json::to_value(request).map_err(|error| {
            Error::Backend(format!("failed to serialize ACP elicitation: {error}"))
        })?;
        if let Some(object) = payload.as_object_mut() {
            object.insert("elicitationId".into(), json!(elicitation_id));
            object.insert("status".into(), json!("pending"));
        }
        self.append_event("elicitation_pending", "The agent needs your input", payload)
    }

    fn record_elicitation_response(
        &self,
        elicitation_id: &str,
        response: &CreateElicitationResponse,
    ) -> Result<()> {
        let mut payload = serde_json::to_value(response).map_err(|error| {
            Error::Backend(format!(
                "failed to serialize ACP elicitation response: {error}"
            ))
        })?;
        let action = payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("cancel")
            .to_string();
        if let Some(object) = payload.as_object_mut() {
            object.insert("elicitationId".into(), json!(elicitation_id));
            object.insert("status".into(), json!("resolved"));
        }
        self.append_event(
            "elicitation_resolved",
            match action.as_str() {
                "accept" => "You answered the agent",
                "decline" => "You skipped the agent's question",
                _ => "The agent's question was cancelled",
            },
            payload,
        )?;

        let sessions = SessionManager::new(self.db.clone());
        let attempt = sessions.get_attempt(&self.attempt_id)?;
        if !matches!(
            attempt.status.as_str(),
            "completed" | "failed" | "cancelled"
        ) {
            sessions.transition_attempt(
                &self.attempt_id,
                "running",
                "Continuing with your answer",
                None,
                None,
            )?;
            TaskBoard::new(self.db.clone()).update_status(
                &self.task_id,
                "in_progress",
                Some(&self.logical_session_id),
            )?;
        }
        Ok(())
    }

    fn record_model_selection(&self, model: &str) -> Result<()> {
        self.append_event(
            "session_config",
            &format!("Using model {model}"),
            json!({ "category": "model", "value": model }),
        )
    }

    fn record_session_controls(
        &self,
        options: &[SessionConfigOption],
        modes: Option<&SessionModeState>,
    ) -> Result<()> {
        let models = advertised_model_choices(options)
            .map(|(_, choices)| choices)
            .unwrap_or_default();
        self.append_event(
            "session_config_options",
            &format!(
                "Agent advertised {} session controls",
                options.len() + usize::from(modes.is_some())
            ),
            json!({
                "config_options": options,
                "modes": modes,
                // Retained for older frontends that only understood model
                // choices before generic ACP controls were exposed.
                "models": models
                    .iter()
                    .map(|(value, name)| json!({ "value": value, "name": name }))
                    .collect::<Vec<_>>()
            }),
        )
    }

    fn flush_thought(&self) -> Result<()> {
        let thought = {
            let mut state = self.state.lock().unwrap();
            std::mem::take(&mut state.pending_thought)
        };
        let thought = thought.trim();
        if !thought.is_empty() {
            self.append_event(
                "agent_thought",
                &truncate_chars(thought, 1_000),
                json!({ "text": thought }),
            )?;
        }
        Ok(())
    }

    fn flush_agent_message(&self) -> Result<()> {
        let (message_id, message) = {
            let mut state = self.state.lock().unwrap();
            if !state.capture_prompt_output {
                return Ok(());
            }
            let message = std::mem::take(&mut state.assistant_text).trim().to_string();
            let message_id = state.current_message_id.take();
            if message.is_empty() {
                return Ok(());
            }
            state.last_assistant_text = message.clone();
            (message_id, message)
        };
        self.append_event(
            "runner_progress",
            &message,
            json!({
                "item_type": "agent_message",
                "message_id": message_id,
            }),
        )
    }

    fn flush_prompt_output(&self) -> Result<()> {
        self.flush_agent_message()?;
        self.flush_thought()
    }

    fn append_event(&self, event_type: &str, summary: &str, payload: Value) -> Result<()> {
        let must_persist = matches!(event_type, "elicitation_pending" | "elicitation_resolved");
        if !must_persist && self.emitted.fetch_add(1, Ordering::Relaxed) >= MAX_EVENTS {
            return Ok(());
        }
        SessionManager::new(self.db.clone()).append_event(
            &self.logical_session_id,
            NewEvent {
                attempt_id: Some(&self.attempt_id),
                task_id: Some(&self.task_id),
                source_type: "acp",
                source_id: Some(&self.runner),
                event_type,
                summary,
                payload,
            },
        )?;
        Ok(())
    }

    fn finish(&self) -> Result<(String, String)> {
        self.flush_thought()?;
        let state = self.state.lock().unwrap();
        let current_message = state.assistant_text.trim();
        let summary = if current_message.is_empty() {
            state.last_assistant_text.clone()
        } else {
            current_message.to_string()
        };
        let diagnostic =
            serde_json::to_string_pretty(&state.transcript).unwrap_or_else(|_| "[]".to_string());
        Ok((summary, truncate_bytes(&diagnostic, MAX_DIAGNOSTIC_BYTES)))
    }
}

/// Run one prompt against an ACP agent attached to an isolated container.
/// Existing sessions are resumed without history replay when possible, then
/// loaded as a compatibility fallback.
pub async fn run_turn(
    attached: AttachedContainer,
    recorder: AcpEventRecorder,
    elicitation_broker: Arc<AcpElicitationBroker>,
    existing_session_id: Option<&str>,
    cwd: &Path,
    prompt: &str,
    options: AcpTurnOptions,
) -> Result<AcpTurnResult> {
    let AcpTurnOptions {
        model,
        session_config: requested_config,
        mcp_servers,
    } = options;
    let AttachedContainer {
        input, mut output, ..
    } = attached;
    let (mut stdout_writer, stdout_reader) = tokio::io::duplex(256 * 1024);
    let stderr = Arc::new(Mutex::new(String::new()));
    let stderr_for_task = stderr.clone();
    let output_task = tokio::spawn(async move {
        while let Some(frame) = output.next().await {
            match frame {
                Ok(LogOutput::StdOut { message }) | Ok(LogOutput::Console { message }) => {
                    if stdout_writer.write_all(&message).await.is_err() {
                        break;
                    }
                }
                Ok(LogOutput::StdErr { message }) => {
                    let mut captured = stderr_for_task.lock().unwrap();
                    if captured.len() < MAX_DIAGNOSTIC_BYTES {
                        captured.push_str(&String::from_utf8_lossy(&message));
                    }
                }
                Ok(LogOutput::StdIn { .. }) => {}
                Err(error) => {
                    let mut captured = stderr_for_task.lock().unwrap();
                    captured.push_str(&format!("\ncontainer stream error: {error}"));
                    break;
                }
            }
        }
    });

    let transport = ByteStreams::new(input.compat_write(), stdout_reader.compat());
    let notification_recorder = recorder.clone();
    let permission_recorder = recorder.clone();
    let elicitation_recorder = recorder.clone();
    let elicitation_task_id = recorder.task_id.clone();
    let elicitation_attempt_id = recorder.attempt_id.clone();
    let prompt_recorder = recorder.clone();
    let existing_session_id = existing_session_id.map(str::to_owned);
    let cwd = cwd.to_path_buf();
    let prompt = prompt.to_string();

    let protocol_result = Client
        .builder()
        .name("xpressclaw")
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                notification_recorder
                    .record_notification(notification)
                    .map_err(agent_client_protocol::Error::into_internal_error)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let selected = request
                    .options
                    .iter()
                    .find(|option| option.kind == PermissionOptionKind::AllowAlways)
                    .or_else(|| {
                        request
                            .options
                            .iter()
                            .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                    });
                permission_recorder
                    .record_permission(&request, selected.map(|option| option.name.as_str()));
                if let Some(option) = selected {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                            option.option_id.clone(),
                        )),
                    ))
                } else {
                    responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ))
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: CreateElicitationRequest, responder, _connection| {
                let (elicitation_id, receiver) = elicitation_broker
                    .begin(&elicitation_task_id, &elicitation_attempt_id);
                if let Err(error) = elicitation_recorder
                    .record_elicitation_pending(&elicitation_id, &request)
                {
                    elicitation_broker.abandon(&elicitation_id);
                    return Err(agent_client_protocol::Error::into_internal_error(error));
                }

                let response = receiver.await.unwrap_or_else(|_| {
                    CreateElicitationResponse::new(ElicitationAction::Cancel)
                });
                elicitation_recorder
                    .record_elicitation_response(&elicitation_id, &response)
                    .map_err(agent_client_protocol::Error::into_internal_error)?;
                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, |connection: ConnectionTo<Agent>| async move {
            let initialized = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
                        ClientCapabilities::new()
                            .session(ClientSessionCapabilities::new().config_options(
                                SessionConfigOptionsCapabilities::new()
                                    .boolean(BooleanConfigOptionCapabilities::new()),
                            ))
                            .elicitation(
                                ElicitationCapabilities::new()
                                    .form(ElicitationFormCapabilities::new()),
                            ),
                    ),
                )
                .block_task()
                .await?;

            for server in &mcp_servers {
                match server {
                    McpServer::Http(server)
                        if !initialized.agent_capabilities.mcp_capabilities.http =>
                    {
                        return Err(agent_client_protocol::util::internal_error(format!(
                            "ACP agent does not support HTTP MCP server '{}'",
                            server.name
                        )));
                    }
                    McpServer::Sse(server)
                        if !initialized.agent_capabilities.mcp_capabilities.sse =>
                    {
                        return Err(agent_client_protocol::util::internal_error(format!(
                            "ACP agent does not support SSE MCP server '{}'",
                            server.name
                        )));
                    }
                    _ => {}
                }
            }

            let (session_id, mut config_options, mut modes) = if let Some(session_id) = existing_session_id {
                if initialized
                    .agent_capabilities
                    .session_capabilities
                    .resume
                    .is_some()
                {
                    let response = connection
                        .send_request(
                            ResumeSessionRequest::new(session_id.clone(), cwd.clone())
                                .mcp_servers(mcp_servers.clone()),
                        )
                        .block_task()
                        .await?;
                    (
                        session_id,
                        response.config_options.unwrap_or_default(),
                        response.modes,
                    )
                } else if initialized.agent_capabilities.load_session {
                    let response = connection
                        .send_request(
                            LoadSessionRequest::new(session_id.clone(), cwd.clone())
                                .mcp_servers(mcp_servers.clone()),
                        )
                        .block_task()
                        .await?;
                    (
                        session_id,
                        response.config_options.unwrap_or_default(),
                        response.modes,
                    )
                } else {
                    return Err(agent_client_protocol::util::internal_error(
                        "ACP agent cannot resume or load an existing session",
                    ));
                }
            } else {
                let response = connection
                    .send_request(
                        NewSessionRequest::new(cwd.clone()).mcp_servers(mcp_servers.clone()),
                    )
                    .block_task()
                    .await?;
                (
                    response.session_id.to_string(),
                    response.config_options.unwrap_or_default(),
                    response.modes,
                )
            };

            prompt_recorder
                .record_session_controls(&config_options, modes.as_ref())
                .map_err(agent_client_protocol::Error::into_internal_error)?;

            if let Some(requested_model) = model.as_deref() {
                let (config_id, model_id) =
                    resolve_model_selection(&config_options, requested_model)
                        .map_err(agent_client_protocol::util::internal_error)?;
                let response = connection
                    .send_request(SetSessionConfigOptionRequest::new(
                        session_id.clone(),
                        config_id,
                        model_id.as_str(),
                    ))
                    .block_task()
                    .await?;
                if !response.config_options.is_empty() {
                    config_options = response.config_options;
                }
                prompt_recorder
                    .record_model_selection(&model_id)
                    .map_err(agent_client_protocol::Error::into_internal_error)?;
            }

            let mut requested_config = requested_config.into_iter().collect::<Vec<_>>();
            requested_config.sort_by(|left, right| left.0.cmp(&right.0));
            for (config_id, value) in requested_config {
                if let Some(option) = config_options
                    .iter()
                    .find(|option| option.id.to_string() == config_id)
                {
                    let option_name = option.name.clone();
                    let value = resolve_config_value(option, &value)
                        .map_err(agent_client_protocol::util::internal_error)?;
                    let display_value = match &value {
                        SessionConfigOptionValue::Boolean { value } => value.to_string(),
                        SessionConfigOptionValue::ValueId { value } => value.to_string(),
                        _ => "updated".to_string(),
                    };
                    let response = connection
                        .send_request(SetSessionConfigOptionRequest::new(
                            session_id.clone(),
                            config_id.clone(),
                            value.clone(),
                        ))
                        .block_task()
                        .await?;
                    if !response.config_options.is_empty() {
                        config_options = response.config_options;
                    }
                    prompt_recorder
                        .append_event(
                            "session_config",
                            &format!("Set {option_name} to {display_value}"),
                            json!({ "config_id": config_id, "value": value }),
                        )
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                    continue;
                }

                if config_id == "mode" {
                    let requested_mode = value.as_str().ok_or_else(|| {
                        agent_client_protocol::util::internal_error(
                            "legacy ACP mode values must be strings",
                        )
                    })?;
                    let available = modes.as_ref().ok_or_else(|| {
                        agent_client_protocol::util::internal_error(format!(
                            "ACP agent does not advertise a session config option or legacy mode named '{config_id}'"
                        ))
                    })?;
                    if !available
                        .available_modes
                        .iter()
                        .any(|mode| mode.id.to_string() == requested_mode)
                    {
                        return Err(agent_client_protocol::util::internal_error(format!(
                            "ACP agent does not offer mode '{requested_mode}'"
                        )));
                    }
                    connection
                        .send_request(SetSessionModeRequest::new(
                            session_id.clone(),
                            requested_mode.to_string(),
                        ))
                        .block_task()
                        .await?;
                    if let Some(modes) = modes.as_mut() {
                        modes.current_mode_id = requested_mode.to_string().into();
                    }
                    prompt_recorder
                        .append_event(
                            "session_mode",
                            &format!("Switched to {requested_mode} mode"),
                            json!({ "mode_id": requested_mode }),
                        )
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                    continue;
                }

                return Err(agent_client_protocol::util::internal_error(format!(
                    "ACP agent does not advertise session configuration '{config_id}'"
                )));
            }

            // `session/load` may replay prior messages. Only output emitted for
            // the prompt below belongs in this attempt's final response.
            prompt_recorder.reset_prompt_output();
            let response = connection
                .send_request(PromptRequest::new(
                    session_id.clone(),
                    vec![ContentBlock::Text(TextContent::new(prompt))],
                ))
                .block_task()
                .await?;

            Ok((session_id.to_string(), response.stop_reason))
        })
        .await;

    output_task.abort();
    let stderr = stderr.lock().unwrap().trim().to_string();
    let (summary, transcript) = recorder.finish()?;
    let (session_id, stop_reason) = protocol_result.map_err(|error| {
        let detail = if stderr.is_empty() {
            error.to_string()
        } else {
            format!("{error}: {stderr}")
        };
        Error::Backend(format!("ACP turn failed: {detail}"))
    })?;
    let stop_reason = serde_json::to_value(stop_reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string());
    let summary = if summary.is_empty() {
        format!("ACP turn finished ({stop_reason})")
    } else {
        summary
    };
    let diagnostic = if stderr.is_empty() {
        transcript
    } else {
        format!(
            "{transcript}\n\nACP stderr:\n{}",
            truncate_bytes(&stderr, 20_000)
        )
    };

    Ok(AcpTurnResult {
        session_id,
        summary,
        stop_reason,
        diagnostic,
    })
}

fn resolve_model_selection(
    config_options: &[SessionConfigOption],
    requested: &str,
) -> std::result::Result<(String, String), String> {
    let Some((config_id, choices)) = advertised_model_choices(config_options) else {
        return Err(format!(
            "ACP agent does not advertise a model configuration option; remove runner.model ({requested}) or choose the model in the agent"
        ));
    };
    let selected = choices.iter().find(|(value, name)| {
        value == requested
            || value.eq_ignore_ascii_case(requested)
            || name.eq_ignore_ascii_case(requested)
    });
    let Some((value, _)) = selected else {
        let available = choices
            .iter()
            .map(|(value, name)| {
                if value == name {
                    value.clone()
                } else {
                    format!("{name} ({value})")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "model '{requested}' is not offered by this ACP agent; available models: {available}"
        ));
    };
    Ok((config_id, value.clone()))
}

fn resolve_config_value(
    option: &SessionConfigOption,
    requested: &Value,
) -> std::result::Result<SessionConfigOptionValue, String> {
    match &option.kind {
        SessionConfigKind::Boolean(_) => requested
            .as_bool()
            .map(SessionConfigOptionValue::boolean)
            .ok_or_else(|| format!("session option '{}' requires a boolean", option.name)),
        SessionConfigKind::Select(select) => {
            let requested = requested.as_str().ok_or_else(|| {
                format!("session option '{}' requires a string value", option.name)
            })?;
            let choices = match &select.options {
                SessionConfigSelectOptions::Ungrouped(options) => options
                    .iter()
                    .map(|option| (option.value.to_string(), option.name.as_str()))
                    .collect::<Vec<_>>(),
                SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|group| group.options.iter())
                    .map(|option| (option.value.to_string(), option.name.as_str()))
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            let selected = choices
                .iter()
                .find(|(value, name)| {
                    value == requested
                        || value.eq_ignore_ascii_case(requested)
                        || name.eq_ignore_ascii_case(requested)
                })
                .map(|(value, _)| value.clone())
                .ok_or_else(|| {
                    format!(
                        "session option '{}' does not offer value '{requested}'",
                        option.name
                    )
                })?;
            Ok(SessionConfigOptionValue::value_id(selected))
        }
        _ => Err(format!(
            "session option '{}' has an unsupported type",
            option.name
        )),
    }
}

fn advertised_model_choices(
    config_options: &[SessionConfigOption],
) -> Option<(String, Vec<(String, String)>)> {
    let model = config_options.iter().find(|option| {
        matches!(option.category, Some(SessionConfigOptionCategory::Model))
            || option.id.to_string().eq_ignore_ascii_case("model")
            || option.name.eq_ignore_ascii_case("model")
    })?;
    let SessionConfigKind::Select(select) = &model.kind else {
        return None;
    };
    let choices: Vec<(String, String)> = match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .iter()
            .map(|option| (option.value.to_string(), option.name.clone()))
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .map(|option| (option.value.to_string(), option.name.clone()))
            .collect(),
        _ => return None,
    };
    Some((model.id.to_string(), choices))
}

fn tool_summary(title: &str, status: ToolCallStatus) -> String {
    match status {
        ToolCallStatus::Pending => format!("Preparing {title}"),
        ToolCallStatus::InProgress => title.to_string(),
        ToolCallStatus::Completed => format!("Completed {title}"),
        ToolCallStatus::Failed => format!("Failed {title}"),
        _ => title.to_string(),
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(max).collect::<String>())
    }
}

fn truncate_bytes(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… truncated …", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        AvailableCommand, AvailableCommandsUpdate, ContentChunk, EnvVariable, InitializeResponse,
        McpServerStdio, NewSessionResponse, Plan, PlanEntry, PlanEntryPriority, PromptResponse,
        SessionConfigSelectOption, SessionMode, SessionModeState, SetSessionConfigOptionResponse,
        StopReason, ToolCall, ToolCallUpdate, ToolCallUpdateFields,
    };
    use bytes::Bytes;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio_stream::wrappers::ReceiverStream;

    use crate::docker::manager::ContainerInfo;

    #[tokio::test]
    async fn acp_turn_uses_the_standard_handshake_and_prompt() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db.clone());
        let (client_input, agent_input) = tokio::io::duplex(64 * 1024);
        let (output_tx, output_rx) = tokio::sync::mpsc::channel(8);

        let mock_agent = tokio::spawn(async move {
            let mut requests = BufReader::new(agent_input).lines();
            while let Some(line) = requests.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let id = request["id"].clone();
                let method = request["method"].as_str().unwrap();
                let response = match method {
                    "initialize" => {
                        assert!(
                            request["params"]["clientCapabilities"]["elicitation"]["form"]
                                .is_object()
                        );
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": InitializeResponse::new(ProtocolVersion::V1),
                        })
                    }
                    "session/new" => {
                        assert_eq!(request["params"]["mcpServers"][0]["name"], "github");
                        assert_eq!(
                            request["params"]["mcpServers"][0]["command"],
                            "/opt/xpressclaw/mcp-github.mjs"
                        );
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": NewSessionResponse::new("acp-session-1").config_options(vec![
                                SessionConfigOption::select(
                                    "model",
                                    "Model",
                                    "model-default",
                                    vec![
                                        SessionConfigSelectOption::new("model-default", "Default"),
                                        SessionConfigSelectOption::new("model-test", "Test Model"),
                                    ],
                                ).category(SessionConfigOptionCategory::Model),
                                SessionConfigOption::boolean(
                                    "use_fast_tools",
                                    "Use fast tools",
                                    false,
                                ),
                            ]).modes(SessionModeState::new(
                                "plan",
                                vec![
                                    SessionMode::new("plan", "Plan"),
                                    SessionMode::new("build", "Build"),
                                ],
                            )),
                        })
                    }
                    "session/set_config_option" => {
                        assert_eq!(request["params"]["sessionId"], "acp-session-1");
                        match request["params"]["configId"].as_str().unwrap() {
                            "model" => assert_eq!(request["params"]["value"], "model-test"),
                            "use_fast_tools" => {
                                assert_eq!(request["params"]["type"], "boolean");
                                assert_eq!(request["params"]["value"], true);
                            }
                            other => panic!("unexpected config option: {other}"),
                        }
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": SetSessionConfigOptionResponse::new(vec![]),
                        })
                    }
                    "session/set_mode" => {
                        assert_eq!(request["params"]["sessionId"], "acp-session-1");
                        assert_eq!(request["params"]["modeId"], "build");
                        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                    }
                    "session/prompt" => {
                        assert_eq!(request["params"]["sessionId"], "acp-session-1");
                        assert_eq!(request["params"]["prompt"][0]["text"], "Do the work");
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "method": "session/update",
                                "params": SessionNotification::new(
                                    "acp-session-1",
                                    SessionUpdate::AvailableCommandsUpdate(
                                        AvailableCommandsUpdate::new(vec![
                                            AvailableCommand::new("loop", "Keep working toward a goal"),
                                        ]),
                                    ),
                                ),
                            }),
                        )
                        .await;
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "method": "session/update",
                                "params": SessionNotification::new(
                                    "acp-session-1",
                                    SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                        ContentBlock::Text(TextContent::new("Work complete")),
                                    )),
                                ),
                            }),
                        )
                        .await;
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": PromptResponse::new(StopReason::EndTurn),
                        })
                    }
                    other => panic!("unexpected ACP method: {other}"),
                };
                send_json(&output_tx, response).await;
                if method == "session/prompt" {
                    break;
                }
            }
        });

        let attached = AttachedContainer {
            info: ContainerInfo {
                container_id: "test-container".to_string(),
                agent_id: "test-attempt".to_string(),
                status: "running".to_string(),
                host_port: None,
            },
            input: Box::pin(client_input),
            output: Box::pin(ReceiverStream::new(output_rx)),
        };
        let result = run_turn(
            attached,
            recorder,
            Arc::new(AcpElicitationBroker::new()),
            None,
            Path::new("/workspace"),
            "Do the work",
            AcpTurnOptions {
                model: Some("Test Model".into()),
                session_config: [
                    ("mode".into(), json!("build")),
                    ("use_fast_tools".into(), json!(true)),
                ]
                .into_iter()
                .collect(),
                mcp_servers: vec![McpServer::Stdio(
                    McpServerStdio::new("github", "/opt/xpressclaw/mcp-github.mjs")
                        .env(vec![EnvVariable::new("GH_REPO", "owner/repo")]),
                )],
            },
        )
        .await
        .unwrap();

        assert_eq!(result.session_id, "acp-session-1");
        assert_eq!(result.stop_reason, "end_turn");
        assert_eq!(result.summary, "Work complete");
        let events = SessionManager::new(db)
            .list_events("session-1", None, 20)
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == "session_config_options"));
        assert!(events
            .iter()
            .any(|event| event.summary == "Using model model-test"));
        assert!(events
            .iter()
            .any(|event| event.summary == "Set Use fast tools to true"));
        assert!(events
            .iter()
            .any(|event| event.summary == "Switched to build mode"));
        assert!(events
            .iter()
            .any(|event| event.event_type == "available_commands"));
        mock_agent.await.unwrap();
    }

    #[tokio::test]
    async fn acp_elicitation_pauses_and_resumes_the_active_prompt() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, task_id) = test_recorder(db.clone());
        let broker = Arc::new(AcpElicitationBroker::new());
        let (client_input, agent_input) = tokio::io::duplex(64 * 1024);
        let (output_tx, output_rx) = tokio::sync::mpsc::channel(8);

        let mock_agent = tokio::spawn(async move {
            let mut requests = BufReader::new(agent_input).lines();
            let mut prompt_id = None;
            while let Some(line) = requests.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                if request.get("method").is_none() && request["id"] == 700 {
                    assert_eq!(request["result"]["action"], "accept");
                    assert_eq!(request["result"]["content"]["question_0"], "PostgreSQL");
                    send_json(
                        &output_tx,
                        json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": SessionNotification::new(
                                "acp-session-questions",
                                SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                    ContentBlock::Text(TextContent::new("Continuing with PostgreSQL")),
                                )),
                            ),
                        }),
                    )
                    .await;
                    send_json(
                        &output_tx,
                        json!({
                            "jsonrpc": "2.0",
                            "id": prompt_id.take().unwrap(),
                            "result": PromptResponse::new(StopReason::EndTurn),
                        }),
                    )
                    .await;
                    break;
                }

                let id = request["id"].clone();
                match request["method"].as_str().unwrap() {
                    "initialize" => {
                        assert!(
                            request["params"]["clientCapabilities"]["elicitation"]["form"]
                                .is_object()
                        );
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": InitializeResponse::new(ProtocolVersion::V1),
                            }),
                        )
                        .await;
                    }
                    "session/new" => {
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": NewSessionResponse::new("acp-session-questions"),
                            }),
                        )
                        .await;
                    }
                    "session/prompt" => {
                        prompt_id = Some(id);
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": 700,
                                "method": "elicitation/create",
                                "params": {
                                    "mode": "form",
                                    "sessionId": "acp-session-questions",
                                    "message": "Which database should I use?",
                                    "requestedSchema": {
                                        "type": "object",
                                        "properties": {
                                            "question_0": {
                                                "type": "string",
                                                "oneOf": [
                                                    { "const": "PostgreSQL", "title": "PostgreSQL" },
                                                    { "const": "SQLite", "title": "SQLite" }
                                                ]
                                            },
                                            "question_0_custom": {
                                                "type": "string",
                                                "title": "Other"
                                            }
                                        }
                                    }
                                }
                            }),
                        )
                        .await;
                    }
                    method => panic!("unexpected ACP method: {method}"),
                }
            }
        });

        let response_db = db.clone();
        let response_broker = broker.clone();
        let response_task_id = task_id.clone();
        let simulated_ui = tokio::spawn(async move {
            for _ in 0..100 {
                let events = SessionManager::new(response_db.clone())
                    .list_events("session-1", None, 50)
                    .unwrap();
                if let Some(event) = events
                    .iter()
                    .find(|event| event.event_type == "elicitation_pending")
                {
                    let elicitation_id = event.payload["elicitationId"].as_str().unwrap();
                    let response = serde_json::from_value(json!({
                        "action": "accept",
                        "content": { "question_0": "PostgreSQL" }
                    }))
                    .unwrap();
                    response_broker
                        .respond(&response_task_id, elicitation_id, response)
                        .unwrap();
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            panic!("elicitation was not persisted");
        });

        let attached = AttachedContainer {
            info: ContainerInfo {
                container_id: "test-container".to_string(),
                agent_id: "test-attempt".to_string(),
                status: "running".to_string(),
                host_port: None,
            },
            input: Box::pin(client_input),
            output: Box::pin(ReceiverStream::new(output_rx)),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            run_turn(
                attached,
                recorder,
                broker,
                None,
                Path::new("/workspace"),
                "Choose the database",
                AcpTurnOptions::default(),
            ),
        )
        .await
        .expect("ACP turn timed out")
        .unwrap();

        assert_eq!(result.summary, "Continuing with PostgreSQL");
        simulated_ui.await.unwrap();
        mock_agent.await.unwrap();
        let events = SessionManager::new(db.clone())
            .list_events("session-1", None, 50)
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == "elicitation_pending"));
        assert!(events
            .iter()
            .any(|event| event.event_type == "elicitation_resolved"));
        assert_eq!(
            TaskBoard::new(db).get(&task_id).unwrap().status,
            TaskStatus::InProgress
        );
    }

    #[tokio::test]
    async fn elicitation_broker_routes_only_the_matching_task_response() {
        let broker = AcpElicitationBroker::new();
        let (elicitation_id, receiver) = broker.begin("task-1", "attempt-1");
        let response: CreateElicitationResponse = serde_json::from_value(json!({
            "action": "accept",
            "content": { "question_0": "PostgreSQL" }
        }))
        .unwrap();

        assert_eq!(
            broker.respond("task-2", &elicitation_id, response.clone()),
            Err(AcpElicitationResponseError::WrongTask)
        );
        broker.respond("task-1", &elicitation_id, response).unwrap();
        let delivered = serde_json::to_value(receiver.await.unwrap()).unwrap();
        assert_eq!(delivered["action"], "accept");
        assert_eq!(delivered["content"]["question_0"], "PostgreSQL");
    }

    #[tokio::test]
    async fn cancelling_an_attempt_releases_its_pending_elicitation() {
        let broker = AcpElicitationBroker::new();
        let (_first_id, first) = broker.begin("task-1", "attempt-1");
        let (_second_id, second) = broker.begin("task-2", "attempt-2");

        assert_eq!(broker.cancel_attempt("attempt-1"), 1);
        let cancelled = serde_json::to_value(first.await.unwrap()).unwrap();
        assert_eq!(cancelled["action"], "cancel");
        assert_eq!(broker.cancel_attempt("attempt-2"), 1);
        assert_eq!(
            serde_json::to_value(second.await.unwrap()).unwrap()["action"],
            "cancel"
        );
    }

    #[test]
    fn model_preferences_are_validated_against_agent_choices() {
        let options = vec![SessionConfigOption::select(
            "model-selector",
            "Language model",
            "sonnet",
            vec![
                SessionConfigSelectOption::new("sonnet", "Sonnet"),
                SessionConfigSelectOption::new("opus", "Opus"),
            ],
        )
        .category(SessionConfigOptionCategory::Model)];

        assert_eq!(
            resolve_model_selection(&options, "Opus").unwrap(),
            ("model-selector".to_string(), "opus".to_string())
        );
        assert!(resolve_model_selection(&options, "unknown")
            .unwrap_err()
            .contains("Sonnet (sonnet), Opus (opus)"));
    }

    #[test]
    fn acp_message_chunks_form_the_final_response() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db);
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new("Implemented "),
                ))),
            ))
            .unwrap();
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                    TextContent::new("the change."),
                ))),
            ))
            .unwrap();

        assert_eq!(recorder.finish().unwrap().0, "Implemented the change.");
    }

    #[test]
    fn acp_agent_updates_are_emitted_before_the_tool_that_follows_them() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db.clone());
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::AgentMessageChunk(
                    ContentChunk::new(ContentBlock::Text(TextContent::new(
                        "Yes, I'll inspect the project.",
                    )))
                    .message_id("status-1"),
                ),
            ))
            .unwrap();
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::ToolCall(
                    ToolCall::new("tool-1", "Read the project").status(ToolCallStatus::InProgress),
                ),
            ))
            .unwrap();
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::AgentMessageChunk(
                    ContentChunk::new(ContentBlock::Text(TextContent::new(
                        "The project is ready.",
                    )))
                    .message_id("final-1"),
                ),
            ))
            .unwrap();

        assert_eq!(recorder.finish().unwrap().0, "The project is ready.");
        let events = SessionManager::new(db)
            .list_events("session-1", None, 50)
            .unwrap();
        let update = events
            .iter()
            .find(|event| event.payload["item_type"] == "agent_message")
            .unwrap();
        let tool = events
            .iter()
            .find(|event| event.event_type == "tool_call")
            .unwrap();
        assert_eq!(update.summary, "Yes, I'll inspect the project.");
        assert_eq!(update.payload["message_id"], "status-1");
        assert!(update.id < tool.id);
        assert!(!events
            .iter()
            .any(|event| event.summary == "The project is ready."));
    }

    #[test]
    fn acp_message_ids_separate_updates_without_a_tool_boundary() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db.clone());
        for (message_id, text) in [("status-1", "Starting now."), ("final-1", "Finished.")] {
            recorder
                .record_notification(SessionNotification::new(
                    "session-1",
                    SessionUpdate::AgentMessageChunk(
                        ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
                            .message_id(message_id),
                    ),
                ))
                .unwrap();
        }

        assert_eq!(recorder.finish().unwrap().0, "Finished.");
        let events = SessionManager::new(db)
            .list_events("session-1", None, 50)
            .unwrap();
        let updates = events
            .iter()
            .filter(|event| event.payload["item_type"] == "agent_message")
            .collect::<Vec<_>>();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].summary, "Starting now.");
    }

    #[test]
    fn acp_tool_updates_are_visible_activity() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db.clone());
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::ToolCall(
                    ToolCall::new("tool-1", "Run tests").status(ToolCallStatus::InProgress),
                ),
            ))
            .unwrap();
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                    "tool-1",
                    ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
                )),
            ))
            .unwrap();

        let events = SessionManager::new(db)
            .list_events("session-1", None, 20)
            .unwrap();
        assert!(events.iter().any(|event| event.summary == "Run tests"));
        assert!(events
            .iter()
            .any(|event| event.summary == "Completed Run tests"));
    }

    #[test]
    fn acp_plans_become_real_subtasks() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, task_id) = test_recorder(db.clone());
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::Plan(Plan::new(vec![
                    PlanEntry::new(
                        "Implement ACP",
                        PlanEntryPriority::High,
                        PlanEntryStatus::InProgress,
                    ),
                    PlanEntry::new(
                        "Run tests",
                        PlanEntryPriority::Medium,
                        PlanEntryStatus::Pending,
                    ),
                ])),
            ))
            .unwrap();

        let subtasks = TaskBoard::new(db).list_subtasks(&task_id).unwrap();
        assert_eq!(subtasks.len(), 2);
        assert_eq!(subtasks[0].title, "Implement ACP");
    }

    fn test_recorder(db: Arc<Database>) -> (AcpEventRecorder, String) {
        let task = TaskBoard::new(db.clone())
            .create(&crate::tasks::board::CreateTask {
                title: "Parent task".to_string(),
                description: None,
                agent_id: Some("session-1".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: Some(0),
                context: None,
            })
            .unwrap();
        let queued = crate::tasks::queue::TaskQueue::new(db.clone())
            .enqueue(&task.id, "session-1")
            .unwrap();
        let attempt_id = queued.attempt_id.unwrap();
        let recorder = AcpEventRecorder::new(db, "session-1", attempt_id, task.id.clone(), "codex");
        recorder.reset_prompt_output();
        (recorder, task.id)
    }

    async fn send_json(
        sender: &tokio::sync::mpsc::Sender<std::result::Result<LogOutput, bollard::errors::Error>>,
        value: Value,
    ) {
        let mut encoded = serde_json::to_vec(&value).unwrap();
        encoded.push(b'\n');
        sender
            .send(Ok(LogOutput::StdOut {
                message: Bytes::from(encoded),
            }))
            .await
            .unwrap();
    }
}
