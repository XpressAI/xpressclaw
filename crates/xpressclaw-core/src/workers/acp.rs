//! Agent Client Protocol transport and event normalization for isolated runners.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub use agent_client_protocol::schema::v1::CreateElicitationResponse;
use agent_client_protocol::schema::v1::{
    BooleanConfigOptionCapabilities, CancelNotification, ClientCapabilities,
    ClientSessionCapabilities, ContentBlock, CreateElicitationRequest, ElicitationAction,
    ElicitationCapabilities, ElicitationFormCapabilities, ForkSessionRequest, ImageContent,
    InitializeRequest, InitializeResponse, LoadSessionRequest, McpServer, NewSessionRequest,
    PermissionOptionKind, PlanEntryStatus, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigOptionValue, SessionConfigOptionsCapabilities, SessionConfigSelectOptions,
    SessionModeState, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionModeRequest, TextContent, ToolCallStatus, ToolKind,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use bollard::container::LogOutput;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::warn;
use uuid::Uuid;

use crate::dashboard::DashboardManager;
use crate::db::Database;
use crate::docker::manager::AttachedContainer;
use crate::error::{Error, Result};
use crate::sessions::{NewEvent, SessionManager};
use crate::tasks::board::{ReportedSubtask, TaskBoard, TaskStatus};
use crate::tasks::conversation::PromptImageAttachment;

const MAX_TRANSCRIPT_UPDATES: usize = 500;
const MAX_DIAGNOSTIC_BYTES: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpInterruptMode {
    /// Finish any tool that is already in flight, then start the queued turn.
    AfterTool,
    /// Stop the current prompt turn as soon as the ACP agent can cancel it.
    Immediate,
}

#[derive(Default)]
struct PendingTurnControl {
    sender: Option<mpsc::UnboundedSender<()>>,
    active_tools: HashSet<String>,
    requested: Option<AcpInterruptMode>,
    sent: bool,
}

/// Routes task-page guidance into an active ACP prompt turn.
///
/// Messages themselves remain durable in task chat and the queue. This broker
/// only owns the short-lived signal that ends the current prompt either now or
/// after its in-flight tool finishes, allowing the queued turn to start early.
#[derive(Default)]
pub struct AcpTurnControlBroker {
    turns: Mutex<HashMap<String, PendingTurnControl>>,
}

impl AcpTurnControlBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a claimed worker before it enters preparation. This lets
    /// guidance received during image setup wait for the ACP turn to connect.
    pub fn begin_attempt(&self, attempt_id: &str) {
        self.turns
            .lock()
            .unwrap()
            .entry(attempt_id.to_string())
            .or_default();
    }

    /// Request that an active attempt yield to its queued continuation.
    /// Returns false when the worker already finished or was already signalled.
    pub fn request_interrupt(&self, attempt_id: &str, mode: AcpInterruptMode) -> bool {
        let mut turns = self.turns.lock().unwrap();
        let Some(control) = turns.get_mut(attempt_id) else {
            return false;
        };
        if control.sent {
            return false;
        }
        if mode == AcpInterruptMode::Immediate
            || control.requested != Some(AcpInterruptMode::Immediate)
        {
            control.requested = Some(mode);
        }
        Self::send_if_ready(control);
        true
    }

    fn register(&self, attempt_id: &str) -> mpsc::UnboundedReceiver<()> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut turns = self.turns.lock().unwrap();
        let control = turns.entry(attempt_id.to_string()).or_default();
        control.sender = Some(sender);
        Self::send_if_ready(control);
        receiver
    }

    fn observe_update(&self, attempt_id: &str, update: &SessionUpdate) {
        let mut turns = self.turns.lock().unwrap();
        let control = turns.entry(attempt_id.to_string()).or_default();
        match update {
            SessionUpdate::ToolCall(call) => {
                Self::set_tool_status(control, call.tool_call_id.to_string(), &call.status)
            }
            SessionUpdate::ToolCallUpdate(update) => {
                if let Some(status) = update.fields.status.as_ref() {
                    Self::set_tool_status(control, update.tool_call_id.to_string(), status);
                }
            }
            _ => {}
        }
        Self::send_if_ready(control);
    }

    fn set_tool_status(
        control: &mut PendingTurnControl,
        tool_call_id: String,
        status: &ToolCallStatus,
    ) {
        if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
            control.active_tools.remove(&tool_call_id);
        } else {
            control.active_tools.insert(tool_call_id);
        }
    }

    fn send_if_ready(control: &mut PendingTurnControl) {
        let ready = match control.requested {
            Some(AcpInterruptMode::Immediate) => true,
            Some(AcpInterruptMode::AfterTool) => control.active_tools.is_empty(),
            None => false,
        };
        if !ready {
            return;
        }
        let Some(sender) = control.sender.as_ref() else {
            return;
        };
        if sender.send(()).is_ok() {
            control.requested = None;
            control.sent = true;
        } else {
            control.sender = None;
        }
    }

    pub fn finish_attempt(&self, attempt_id: &str) {
        self.turns.lock().unwrap().remove(attempt_id);
    }
}

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
/// used with ACP session lifecycle methods on later attempts.
#[derive(Debug)]
pub struct AcpTurnResult {
    pub session_id: String,
    pub summary: String,
    pub stop_reason: String,
    pub diagnostic: String,
    /// True when XpressClaw asked this turn to yield to newer user guidance.
    pub interrupted: bool,
}

/// How a task should establish its ACP conversation before sending a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpSessionStart {
    /// Create a conversation without inherited agent context.
    New,
    /// Continue a conversation already owned by this task.
    Resume(String),
    /// Branch from another task's conversation when the agent supports it.
    Fork(String),
}

/// Per-turn client choices applied after ACP session setup and before its
/// prompt is sent.
#[derive(Debug, Default)]
pub struct AcpTurnOptions {
    pub model: Option<String>,
    pub session_config: HashMap<String, Value>,
    pub mcp_servers: Vec<McpServer>,
    /// Extra workspace roots advertised through ACP. Codex ACP maps a root's
    /// `.agents/skills` directory into its session-scoped skill discovery.
    pub additional_directories: Vec<PathBuf>,
    /// Optional fingerprint for an out-of-band MCP bridge. ACP agents normally
    /// receive their MCP configuration in `session/*`; adapters such as Pi's
    /// load it in the underlying agent process instead. The fingerprint still
    /// makes a changed configuration reload the native session.
    pub mcp_signature: Option<String>,
    pub image_attachments: Vec<PromptImageAttachment>,
}

/// Live services associated with one ACP prompt turn.
pub struct AcpTurnRuntime {
    recorder: AcpEventRecorder,
    elicitation_broker: Arc<AcpElicitationBroker>,
    turn_controls: Arc<AcpTurnControlBroker>,
    allow_elicitation: bool,
}

/// A live, initialized ACP process. One handle is retained for each project so
/// ordinary prompt turns reuse both the container process and its protocol
/// connection instead of repeating startup and authentication work.
#[derive(Clone)]
pub struct AcpProcess {
    sender: mpsc::Sender<AcpProcessTurn>,
    shutdown: watch::Sender<bool>,
    alive: Arc<AtomicBool>,
    stopped: Arc<Notify>,
}

struct AcpProcessTurn {
    runtime: AcpTurnRuntime,
    session_start: AcpSessionStart,
    cwd: std::path::PathBuf,
    prompt: String,
    options: AcpTurnOptions,
    response: oneshot::Sender<Result<AcpTurnResult>>,
}

#[derive(Clone)]
struct ActiveTurn {
    recorder: AcpEventRecorder,
    elicitation_broker: Arc<AcpElicitationBroker>,
    turn_controls: Arc<AcpTurnControlBroker>,
    allow_elicitation: bool,
}

#[derive(Clone)]
struct ConnectedSession {
    config_options: Vec<SessionConfigOption>,
    modes: Option<SessionModeState>,
    mcp_signature: String,
}

impl AcpTurnRuntime {
    pub fn new(
        recorder: AcpEventRecorder,
        elicitation_broker: Arc<AcpElicitationBroker>,
        turn_controls: Arc<AcpTurnControlBroker>,
    ) -> Self {
        Self {
            recorder,
            elicitation_broker,
            turn_controls,
            allow_elicitation: true,
        }
    }

    pub fn for_conversation(
        recorder: AcpEventRecorder,
        elicitation_broker: Arc<AcpElicitationBroker>,
        turn_controls: Arc<AcpTurnControlBroker>,
    ) -> Self {
        Self {
            recorder,
            elicitation_broker,
            turn_controls,
            allow_elicitation: false,
        }
    }
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
    conversation_id: Option<String>,
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
            conversation_id: None,
            state: Arc::new(Mutex::new(TurnState::default())),
        }
    }

    pub fn for_conversation(
        db: Arc<Database>,
        conversation_id: impl Into<String>,
        agent_id: impl Into<String>,
        turn_id: impl Into<String>,
        runner: impl Into<String>,
    ) -> Self {
        let conversation_id = conversation_id.into();
        Self {
            db,
            logical_session_id: agent_id.into(),
            attempt_id: turn_id.into(),
            task_id: format!("conversation:{conversation_id}"),
            runner: runner.into(),
            conversation_id: Some(conversation_id),
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

    fn persist_native_session(&self, native_session_id: &str) -> Result<()> {
        if self.conversation_id.is_some() {
            return Ok(());
        }
        SessionManager::new(self.db.clone()).set_native_session(&self.attempt_id, native_session_id)
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
                let dashboard_summary = dashboard_tool_summary(call.kind);
                let dashboard = DashboardManager::new(self.db.clone());
                let telemetry = if let Some(conversation_id) = self.conversation_id.as_deref() {
                    dashboard.record_conversation_tool_call(
                        &self.attempt_id,
                        conversation_id,
                        &self.logical_session_id,
                        dashboard_summary,
                    )
                } else {
                    dashboard.record_task_tool_call(
                        &self.attempt_id,
                        &self.task_id,
                        dashboard_summary,
                    )
                };
                if let Err(error) = telemetry {
                    warn!(%error, "failed to record dashboard tool-call telemetry");
                }
                let work_kind = if self.conversation_id.is_some() {
                    "conversation_turn"
                } else {
                    "attempt"
                };
                if let Err(error) =
                    dashboard.record_git_snapshot(work_kind, &self.attempt_id, false)
                {
                    warn!(%error, "failed to record dashboard Git metric boundary");
                }
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
                if self.conversation_id.is_none() {
                    TaskBoard::new(self.db.clone()).sync_reported_subtasks(
                        &self.task_id,
                        &self.attempt_id,
                        &reported,
                    )?;
                }
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
            SessionUpdate::UsageUpdate(update) => {
                self.flush_prompt_output()?;
                if self.conversation_id.is_some() {
                    crate::conversations::runtime::ConversationTurnQueue::new(self.db.clone())
                        .set_context_usage(&self.attempt_id, update.used, update.size)?;
                } else {
                    SessionManager::new(self.db.clone()).set_context_usage(
                        &self.attempt_id,
                        update.used,
                        update.size,
                    )?;
                }
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
            "completed" | "failed" | "cancelled" | "interrupted"
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
        if self.conversation_id.is_some() {
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

impl AcpProcess {
    /// Start and initialize one ACP process over an attached Agent container.
    pub async fn start(attached: AttachedContainer) -> Result<Self> {
        let (sender, receiver) = mpsc::channel(1);
        let (shutdown, shutdown_receiver) = watch::channel(false);
        let (ready_sender, ready_receiver) = oneshot::channel();
        let alive = Arc::new(AtomicBool::new(true));
        let stopped = Arc::new(Notify::new());
        tokio::spawn(serve_process(
            attached,
            receiver,
            shutdown_receiver,
            ready_sender,
            alive.clone(),
            stopped.clone(),
        ));

        ready_receiver.await.map_err(|_| {
            Error::Backend("ACP process exited before initialization completed".to_string())
        })??;

        Ok(Self {
            sender,
            shutdown,
            alive,
            stopped,
        })
    }

    /// Whether the underlying protocol connection is still available.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst) && !self.sender.is_closed()
    }

    /// Whether two handles refer to the same underlying process.
    pub fn same_process(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.alive, &other.alive)
    }

    /// Close this ACP connection without stopping its shared project
    /// container. Conversation lanes use this when their conversation or
    /// participant is deleted while the base project process remains alive.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    /// Wait until the stdio connection and its process actor have exited.
    pub async fn wait_for_exit(&self) {
        while self.alive.load(Ordering::SeqCst) {
            self.stopped.notified().await;
        }
    }

    /// Run one serialized prompt through this already initialized process.
    pub async fn run_turn(
        &self,
        runtime: AcpTurnRuntime,
        session_start: AcpSessionStart,
        cwd: &Path,
        prompt: &str,
        options: AcpTurnOptions,
    ) -> Result<AcpTurnResult> {
        if !self.is_alive() {
            return Err(Error::Backend(
                "ACP process is no longer running".to_string(),
            ));
        }
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(AcpProcessTurn {
                runtime,
                session_start,
                cwd: cwd.to_path_buf(),
                prompt: prompt.to_string(),
                options,
                response,
            })
            .await
            .map_err(|_| {
                Error::Backend("ACP process stopped before the turn started".to_string())
            })?;
        receiver
            .await
            .map_err(|_| Error::Backend("ACP process stopped during the turn".to_string()))?
    }
}

async fn serve_process(
    attached: AttachedContainer,
    mut turns: mpsc::Receiver<AcpProcessTurn>,
    mut shutdown: watch::Receiver<bool>,
    ready_sender: oneshot::Sender<Result<()>>,
    alive: Arc<AtomicBool>,
    stopped: Arc<Notify>,
) {
    let AttachedContainer {
        input, mut output, ..
    } = attached;
    let (mut stdout_writer, stdout_reader) = tokio::io::duplex(256 * 1024);
    let stderr = Arc::new(Mutex::new(String::new()));
    let stderr_for_task = stderr.clone();
    let (output_closed_sender, mut output_closed_receiver) = oneshot::channel();
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
        let _ = output_closed_sender.send(());
    });

    let transport = ByteStreams::new(input.compat_write(), stdout_reader.compat());
    let active_turn = Arc::new(Mutex::new(None::<ActiveTurn>));
    let notification_turn = active_turn.clone();
    let permission_turn = active_turn.clone();
    let elicitation_turn = active_turn.clone();
    let ready = Arc::new(Mutex::new(Some(ready_sender)));
    let ready_for_connection = ready.clone();
    let stderr_for_connection = stderr.clone();

    let protocol_result = Client
        .builder()
        .name("xpressclaw")
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                let active = notification_turn.lock().unwrap().clone();
                if let Some(active) = active {
                    active
                        .turn_controls
                        .observe_update(&active.recorder.attempt_id, &notification.update);
                    active
                        .recorder
                        .record_notification(notification)
                        .map_err(agent_client_protocol::Error::into_internal_error)?;
                }
                Ok(())
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
                if let Some(active) = permission_turn.lock().unwrap().clone() {
                    active
                        .recorder
                        .record_permission(&request, selected.map(|option| option.name.as_str()));
                }
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
                let active = elicitation_turn.lock().unwrap().clone();
                let Some(active) = active else {
                    return responder
                        .respond(CreateElicitationResponse::new(ElicitationAction::Cancel));
                };
                if !active.allow_elicitation {
                    return responder
                        .respond(CreateElicitationResponse::new(ElicitationAction::Cancel));
                }
                let elicitation_task_id = active.recorder.task_id.clone();
                let elicitation_attempt_id = active.recorder.attempt_id.clone();
                let (elicitation_id, receiver) = active
                    .elicitation_broker
                    .begin(&elicitation_task_id, &elicitation_attempt_id);
                if let Err(error) = active
                    .recorder
                    .record_elicitation_pending(&elicitation_id, &request)
                {
                    active.elicitation_broker.abandon(&elicitation_id);
                    return Err(agent_client_protocol::Error::into_internal_error(error));
                }

                let response = receiver
                    .await
                    .unwrap_or_else(|_| CreateElicitationResponse::new(ElicitationAction::Cancel));
                active
                    .recorder
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
                            .session(
                                ClientSessionCapabilities::new().config_options(
                                    SessionConfigOptionsCapabilities::new()
                                        .boolean(BooleanConfigOptionCapabilities::new()),
                                ),
                            )
                            .elicitation(
                                ElicitationCapabilities::new()
                                    .form(ElicitationFormCapabilities::new()),
                            ),
                    ),
                )
                .block_task()
                .await?;

            if let Some(sender) = ready_for_connection.lock().unwrap().take() {
                let _ = sender.send(Ok(()));
            }

            let mut sessions = HashMap::new();
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let turn = tokio::select! {
                    biased;
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            break;
                        }
                        continue;
                    },
                    _ = &mut output_closed_receiver => break,
                    turn = turns.recv() => {
                        let Some(turn) = turn else {
                            break;
                        };
                        turn
                    }
                };
                let AcpProcessTurn {
                    runtime,
                    session_start,
                    cwd,
                    prompt,
                    options,
                    response,
                } = turn;
                let AcpTurnRuntime {
                    recorder,
                    elicitation_broker,
                    turn_controls,
                    allow_elicitation,
                } = runtime;
                *active_turn.lock().unwrap() = Some(ActiveTurn {
                    recorder: recorder.clone(),
                    elicitation_broker,
                    turn_controls: turn_controls.clone(),
                    allow_elicitation,
                });
                stderr_for_connection.lock().unwrap().clear();

                let result = run_connected_turn(
                    &connection,
                    &initialized,
                    &mut sessions,
                    &recorder,
                    &turn_controls,
                    session_start,
                    cwd,
                    prompt,
                    options,
                )
                .await;
                *active_turn.lock().unwrap() = None;
                let turn_stderr = std::mem::take(&mut *stderr_for_connection.lock().unwrap());

                match result {
                    Ok((session_id, stop_reason, interrupted)) => {
                        let result = recorder.finish().map(|(summary, transcript)| {
                            let summary = if summary.is_empty() {
                                format!("ACP turn finished ({stop_reason})")
                            } else {
                                summary
                            };
                            let diagnostic = if turn_stderr.trim().is_empty() {
                                transcript
                            } else {
                                format!(
                                    "{transcript}\n\nACP stderr:\n{}",
                                    truncate_bytes(turn_stderr.trim(), 20_000)
                                )
                            };
                            AcpTurnResult {
                                session_id,
                                summary,
                                stop_reason,
                                diagnostic,
                                interrupted,
                            }
                        });
                        let _ = response.send(result);
                    }
                    Err(error) => {
                        let detail = if turn_stderr.trim().is_empty() {
                            error.to_string()
                        } else {
                            format!("{error}: {}", turn_stderr.trim())
                        };
                        let _ = response
                            .send(Err(Error::Backend(format!("ACP turn failed: {detail}"))));
                        return Err(error);
                    }
                }
            }

            Ok(())
        })
        .await;

    output_task.abort();
    alive.store(false, Ordering::SeqCst);
    stopped.notify_one();
    if let Some(sender) = ready.lock().unwrap().take() {
        let stderr = stderr.lock().unwrap().trim().to_string();
        let detail = match protocol_result.as_ref() {
            Ok(_) if stderr.is_empty() => "ACP process exited during initialization".to_string(),
            Ok(_) => format!("ACP process exited during initialization: {stderr}"),
            Err(error) if stderr.is_empty() => error.to_string(),
            Err(error) => format!("{error}: {stderr}"),
        };
        let _ = sender.send(Err(Error::Backend(detail)));
    }
    if let Err(error) = protocol_result {
        warn!(%error, "persistent ACP process connection stopped");
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_connected_turn(
    connection: &ConnectionTo<Agent>,
    initialized: &InitializeResponse,
    sessions: &mut HashMap<String, ConnectedSession>,
    recorder: &AcpEventRecorder,
    turn_controls: &Arc<AcpTurnControlBroker>,
    session_start: AcpSessionStart,
    cwd: std::path::PathBuf,
    prompt: String,
    options: AcpTurnOptions,
) -> std::result::Result<(String, String, bool), agent_client_protocol::Error> {
    let AcpTurnOptions {
        model,
        session_config: requested_config,
        mcp_servers,
        mcp_signature,
        additional_directories,
        image_attachments,
    } = options;

    if !image_attachments.is_empty() && !initialized.agent_capabilities.prompt_capabilities.image {
        return Err(agent_client_protocol::util::internal_error(
            "the selected ACP agent does not support image prompts",
        ));
    }

    for server in &mcp_servers {
        match server {
            McpServer::Http(server) if !initialized.agent_capabilities.mcp_capabilities.http => {
                return Err(agent_client_protocol::util::internal_error(format!(
                    "ACP agent does not support HTTP MCP server '{}'",
                    server.name
                )));
            }
            McpServer::Sse(server) if !initialized.agent_capabilities.mcp_capabilities.sse => {
                return Err(agent_client_protocol::util::internal_error(format!(
                    "ACP agent does not support SSE MCP server '{}'",
                    server.name
                )));
            }
            _ => {}
        }
    }

    let mcp_signature = match mcp_signature {
        Some(signature) => signature,
        None => serde_json::to_string(&mcp_servers).map_err(|error| {
            agent_client_protocol::util::internal_error(format!(
                "failed to fingerprint ACP MCP configuration: {error}"
            ))
        })?,
    };
    let mcp_signature =
        serde_json::to_string(&(mcp_signature, &additional_directories)).map_err(|error| {
            agent_client_protocol::util::internal_error(format!(
                "failed to fingerprint ACP session roots: {error}"
            ))
        })?;
    let (prepared_session, session_to_resume) = match session_start {
        AcpSessionStart::New => {
            let response = connection
                .send_request(
                    NewSessionRequest::new(cwd.clone())
                        .mcp_servers(mcp_servers.clone())
                        .additional_directories(additional_directories.clone()),
                )
                .block_task()
                .await?;
            (
                Some((
                    response.session_id.to_string(),
                    response.config_options.unwrap_or_default(),
                    response.modes,
                )),
                None,
            )
        }
        AcpSessionStart::Resume(session_id) => (None, Some(session_id)),
        AcpSessionStart::Fork(source_session_id) => {
            if initialized
                .agent_capabilities
                .session_capabilities
                .fork
                .is_some()
            {
                match connection
                    .send_request(
                        ForkSessionRequest::new(source_session_id.clone(), cwd.clone())
                            .mcp_servers(mcp_servers.clone())
                            .additional_directories(additional_directories.clone()),
                    )
                    .block_task()
                    .await
                {
                    Ok(response) => {
                        let forked_session_id = response.session_id.to_string();
                        recorder
                            .append_event(
                                "session_fork",
                                "Forked the inherited agent conversation for this task",
                                json!({
                                    "source_session_id": source_session_id,
                                    "session_id": forked_session_id,
                                }),
                            )
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        (
                            Some((
                                forked_session_id,
                                response.config_options.unwrap_or_default(),
                                response.modes,
                            )),
                            None,
                        )
                    }
                    Err(error) => {
                        warn!(
                            %error,
                            "ACP agent rejected session fork; continuing the source conversation"
                        );
                        recorder
                            .append_event(
                                "session_fork_fallback",
                                "The agent could not fork this conversation; continued it instead",
                                json!({
                                    "source_session_id": source_session_id,
                                    "reason": "fork_failed",
                                }),
                            )
                            .map_err(agent_client_protocol::Error::into_internal_error)?;
                        (None, Some(source_session_id))
                    }
                }
            } else {
                recorder
                    .append_event(
                        "session_fork_fallback",
                        "This agent does not support conversation forks; continued it instead",
                        json!({
                            "source_session_id": source_session_id,
                            "reason": "unsupported",
                        }),
                    )
                    .map_err(agent_client_protocol::Error::into_internal_error)?;
                (None, Some(source_session_id))
            }
        }
    };

    let (session_id, mut config_options, mut modes) =
        if let Some(prepared_session) = prepared_session {
            prepared_session
        } else {
            let Some(session_id) = session_to_resume else {
                return Err(agent_client_protocol::util::internal_error(
                    "ACP session setup did not create or select a session",
                ));
            };
            if let Some(session) = sessions
                .get(&session_id)
                .filter(|session| session.mcp_signature == mcp_signature)
                .cloned()
            {
                (session_id, session.config_options, session.modes)
            } else if initialized
                .agent_capabilities
                .session_capabilities
                .resume
                .is_some()
            {
                let response = connection
                    .send_request(
                        ResumeSessionRequest::new(session_id.clone(), cwd.clone())
                            .mcp_servers(mcp_servers.clone())
                            .additional_directories(additional_directories.clone()),
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
                            .mcp_servers(mcp_servers.clone())
                            .additional_directories(additional_directories.clone()),
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
        };

    let native_session_id = session_id.to_string();
    recorder
        .record_session_controls(&config_options, modes.as_ref())
        .map_err(agent_client_protocol::Error::into_internal_error)?;

    if let Some(requested_model) = model.as_deref() {
        let (config_id, model_id) = resolve_model_selection(&config_options, requested_model)
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
        recorder
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
            recorder
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
            recorder
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

    sessions.insert(
        native_session_id.clone(),
        ConnectedSession {
            config_options,
            modes,
            mcp_signature,
        },
    );

    recorder.reset_prompt_output();
    let mut prompt_blocks = Vec::with_capacity(1 + image_attachments.len());
    if !prompt.trim().is_empty() {
        prompt_blocks.push(ContentBlock::Text(TextContent::new(prompt)));
    }
    prompt_blocks.extend(image_attachments.into_iter().map(|attachment| {
        ContentBlock::Image(ImageContent::new(
            STANDARD.encode(attachment.data),
            attachment.mime_type,
        ))
    }));
    let prompt_request =
        connection.send_request(PromptRequest::new(session_id.clone(), prompt_blocks));
    recorder
        .persist_native_session(&native_session_id)
        .map_err(agent_client_protocol::Error::into_internal_error)?;
    let mut prompt_response = Box::pin(prompt_request.block_task());
    let mut interrupt_receiver = turn_controls.register(&recorder.attempt_id);
    let interrupt_sent = Arc::new(AtomicBool::new(false));
    let interrupt_sent_for_turn = interrupt_sent.clone();
    let mut controls_open = true;
    let response = loop {
        tokio::select! {
            response = &mut prompt_response => break response,
            interrupt = interrupt_receiver.recv(), if controls_open => {
                match interrupt {
                    Some(()) => {
                        interrupt_sent_for_turn.store(true, Ordering::SeqCst);
                        connection.send_notification(CancelNotification::new(
                            session_id.clone(),
                        ))?;
                        controls_open = false;
                    }
                    None => controls_open = false,
                }
            }
        }
    };
    let interrupted = interrupt_sent.load(Ordering::SeqCst);
    let response = match response {
        Ok(response) => response,
        Err(_) if interrupted => {
            return Ok((native_session_id, "cancelled".to_string(), true));
        }
        Err(error) => return Err(error),
    };
    let stop_reason = serde_json::to_value(response.stop_reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string());

    Ok((native_session_id, stop_reason, interrupted))
}

/// Run one prompt against a newly attached ACP process.
///
/// Native project workers retain an AcpProcess and call its method directly;
/// this convenience wrapper remains useful for isolated protocol tests.
pub async fn run_turn(
    attached: AttachedContainer,
    runtime: AcpTurnRuntime,
    session_start: AcpSessionStart,
    cwd: &Path,
    prompt: &str,
    options: AcpTurnOptions,
) -> Result<AcpTurnResult> {
    AcpProcess::start(attached)
        .await?
        .run_turn(runtime, session_start, cwd, prompt, options)
        .await
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
        ToolCallStatus::Completed => title.to_string(),
        ToolCallStatus::Failed => format!("Failed {title}"),
        _ => title.to_string(),
    }
}

fn dashboard_tool_summary(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "Reading workspace data",
        ToolKind::Edit => "Editing workspace files",
        ToolKind::Delete => "Removing workspace content",
        ToolKind::Move => "Moving workspace content",
        ToolKind::Search => "Searching workspace data",
        ToolKind::Execute => "Running a command",
        ToolKind::Think => "Using an internal planning tool",
        ToolKind::Fetch => "Fetching external data",
        ToolKind::SwitchMode => "Switching Agent mode",
        ToolKind::Other => "Using an Agent tool",
        _ => "Using an Agent tool",
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
        AvailableCommand, AvailableCommandsUpdate, ContentChunk, EnvVariable, ForkSessionResponse,
        InitializeResponse, McpServerStdio, NewSessionResponse, Plan, PlanEntry, PlanEntryPriority,
        PromptResponse, ResumeSessionResponse, SessionConfigSelectOption, SessionMode,
        SessionModeState, SetSessionConfigOptionResponse, StopReason, ToolCall, ToolCallUpdate,
        ToolCallUpdateFields,
    };
    use bytes::Bytes;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio_stream::wrappers::ReceiverStream;

    use crate::docker::manager::ContainerInfo;

    #[test]
    fn queued_guidance_interrupts_after_the_active_tool_finishes() {
        let broker = AcpTurnControlBroker::new();
        broker.begin_attempt("attempt-1");
        let mut receiver = broker.register("attempt-1");
        broker.observe_update(
            "attempt-1",
            &SessionUpdate::ToolCall(
                ToolCall::new("tool-1", "Run tests").status(ToolCallStatus::InProgress),
            ),
        );

        assert!(broker.request_interrupt("attempt-1", AcpInterruptMode::AfterTool));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        broker.observe_update(
            "attempt-1",
            &SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                "tool-1",
                ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
            )),
        );
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(!broker.request_interrupt("attempt-1", AcpInterruptMode::Immediate));
    }

    #[test]
    fn immediate_interrupt_is_retained_until_the_turn_connects() {
        let broker = AcpTurnControlBroker::new();
        broker.begin_attempt("attempt-1");
        assert!(broker.request_interrupt("attempt-1", AcpInterruptMode::Immediate));
        let mut receiver = broker.register("attempt-1");
        assert_eq!(receiver.try_recv(), Ok(()));
    }

    #[tokio::test]
    async fn explicitly_shutting_down_an_idle_acp_process_closes_its_actor() {
        let (client_input, agent_input) = tokio::io::duplex(64 * 1024);
        let (output_tx, output_rx) = tokio::sync::mpsc::channel(8);
        let mock_agent = tokio::spawn(async move {
            let mut requests = BufReader::new(agent_input).lines();
            let initialize = requests.next_line().await.unwrap().unwrap();
            let initialize: Value = serde_json::from_str(&initialize).unwrap();
            assert_eq!(initialize["method"], "initialize");
            send_json(
                &output_tx,
                json!({
                    "jsonrpc": "2.0",
                    "id": initialize["id"],
                    "result": InitializeResponse::new(ProtocolVersion::V1),
                }),
            )
            .await;
            assert!(requests.next_line().await.unwrap().is_none());
        });
        let process = AcpProcess::start(AttachedContainer {
            info: ContainerInfo {
                container_id: "test-container".into(),
                agent_id: "test-project".into(),
                status: "running".into(),
                host_port: None,
            },
            input: Box::pin(client_input),
            output: Box::pin(ReceiverStream::new(output_rx)),
        })
        .await
        .unwrap();

        process.shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(1), process.wait_for_exit())
            .await
            .expect("explicit shutdown should stop an idle ACP actor");
        assert!(!process.is_alive());
        mock_agent.await.unwrap();
    }

    #[tokio::test]
    async fn acp_turn_sends_the_standard_cancel_notification() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db.clone());
        let attempt_id = recorder.attempt_id.clone();
        let controls = Arc::new(AcpTurnControlBroker::new());
        controls.begin_attempt(&attempt_id);
        assert!(controls.request_interrupt(&attempt_id, AcpInterruptMode::Immediate));
        let (client_input, agent_input) = tokio::io::duplex(64 * 1024);
        let (output_tx, output_rx) = tokio::sync::mpsc::channel(8);

        let mock_agent = tokio::spawn(async move {
            let mut requests = BufReader::new(agent_input).lines();
            while let Some(line) = requests.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let id = request["id"].clone();
                match request["method"].as_str().unwrap() {
                    "initialize" => {
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
                                "result": NewSessionResponse::new("acp-session-cancel"),
                            }),
                        )
                        .await;
                    }
                    "session/prompt" => {
                        let line = requests.next_line().await.unwrap().unwrap();
                        let cancellation: Value = serde_json::from_str(&line).unwrap();
                        assert_eq!(cancellation["method"], "session/cancel");
                        assert_eq!(cancellation["params"]["sessionId"], "acp-session-cancel");
                        assert!(cancellation.get("id").is_none());
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": PromptResponse::new(StopReason::Cancelled),
                            }),
                        )
                        .await;
                        break;
                    }
                    other => panic!("unexpected ACP method: {other}"),
                }
            }
        });

        let result = run_turn(
            AttachedContainer {
                info: ContainerInfo {
                    container_id: "test-container".to_string(),
                    agent_id: "test-attempt".to_string(),
                    status: "running".to_string(),
                    host_port: None,
                },
                input: Box::pin(client_input),
                output: Box::pin(ReceiverStream::new(output_rx)),
            },
            AcpTurnRuntime::new(recorder, Arc::new(AcpElicitationBroker::new()), controls),
            AcpSessionStart::New,
            Path::new("/workspace"),
            "Do the work",
            AcpTurnOptions::default(),
        )
        .await
        .unwrap();

        assert!(result.interrupted);
        assert_eq!(result.stop_reason, "cancelled");
        assert_eq!(
            SessionManager::new(db)
                .get_attempt(&attempt_id)
                .unwrap()
                .native_session_id
                .as_deref(),
            Some("acp-session-cancel")
        );
        mock_agent.await.unwrap();
    }

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
                        let mut result =
                            serde_json::to_value(InitializeResponse::new(ProtocolVersion::V1))
                                .unwrap();
                        result["agentCapabilities"] = json!({
                            "promptCapabilities": { "image": true }
                        });
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": result,
                        })
                    }
                    "session/new" => {
                        assert_eq!(request["params"]["mcpServers"][0]["name"], "github");
                        assert_eq!(
                            request["params"]["mcpServers"][0]["command"],
                            "/opt/xpressclaw/mcp-github.mjs"
                        );
                        assert_eq!(
                            request["params"]["additionalDirectories"],
                            json!(["/opt/xpressclaw/presentation-runtime"])
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
                        assert_eq!(request["params"]["prompt"][1]["type"], "image");
                        assert_eq!(request["params"]["prompt"][1]["mimeType"], "image/png");
                        assert_eq!(
                            request["params"]["prompt"][1]["data"],
                            STANDARD.encode(b"image bytes")
                        );
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
            AcpTurnRuntime::new(
                recorder,
                Arc::new(AcpElicitationBroker::new()),
                Arc::new(AcpTurnControlBroker::new()),
            ),
            AcpSessionStart::New,
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
                additional_directories: vec![PathBuf::from("/opt/xpressclaw/presentation-runtime")],
                mcp_signature: None,
                image_attachments: vec![PromptImageAttachment {
                    name: "screen.png".into(),
                    mime_type: "image/png".into(),
                    data: b"image bytes".to_vec(),
                }],
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
    async fn acp_process_reuses_its_connection_and_live_session_across_turns() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (first_recorder, _) = test_recorder(db.clone());
        let (second_recorder, _) = test_recorder(db);
        let (client_input, agent_input) = tokio::io::duplex(64 * 1024);
        let (output_tx, output_rx) = tokio::sync::mpsc::channel(8);

        let mock_agent = tokio::spawn(async move {
            let mut requests = BufReader::new(agent_input).lines();
            let mut initialize_count = 0;
            let mut new_count = 0;
            let mut prompt_count = 0;
            while let Some(line) = requests.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let id = request["id"].clone();
                match request["method"].as_str().unwrap() {
                    "initialize" => {
                        initialize_count += 1;
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
                        new_count += 1;
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": NewSessionResponse::new("shared-session"),
                            }),
                        )
                        .await;
                    }
                    "session/prompt" => {
                        prompt_count += 1;
                        assert_eq!(request["params"]["sessionId"], "shared-session");
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": PromptResponse::new(StopReason::EndTurn),
                            }),
                        )
                        .await;
                        if prompt_count == 2 {
                            break;
                        }
                    }
                    other => panic!("unexpected ACP method between turns: {other}"),
                }
            }
            assert_eq!(initialize_count, 1);
            assert_eq!(new_count, 1);
            assert_eq!(prompt_count, 2);
        });

        let process = AcpProcess::start(AttachedContainer {
            info: ContainerInfo {
                container_id: "test-container".to_string(),
                agent_id: "test-project".to_string(),
                status: "running".to_string(),
                host_port: None,
            },
            input: Box::pin(client_input),
            output: Box::pin(ReceiverStream::new(output_rx)),
        })
        .await
        .unwrap();
        let broker = Arc::new(AcpElicitationBroker::new());
        let controls = Arc::new(AcpTurnControlBroker::new());
        let first = process
            .run_turn(
                AcpTurnRuntime::new(first_recorder, broker.clone(), controls.clone()),
                AcpSessionStart::New,
                Path::new("/workspace"),
                "First prompt",
                AcpTurnOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(first.session_id, "shared-session");
        assert!(process.is_alive());

        let second = process
            .run_turn(
                AcpTurnRuntime::new(second_recorder, broker.clone(), controls.clone()),
                AcpSessionStart::Resume(first.session_id.clone()),
                Path::new("/workspace"),
                "Second prompt",
                AcpTurnOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(second.session_id, "shared-session");
        mock_agent.await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), process.wait_for_exit())
            .await
            .expect("ACP process should observe the closed container stream");
        assert!(!process.is_alive());
    }

    #[tokio::test]
    async fn out_of_band_mcp_and_additional_directory_changes_reload_a_live_session() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (first_recorder, _) = test_recorder(db.clone());
        let (second_recorder, _) = test_recorder(db.clone());
        let (third_recorder, _) = test_recorder(db);
        let (client_input, agent_input) = tokio::io::duplex(64 * 1024);
        let (output_tx, output_rx) = tokio::sync::mpsc::channel(8);

        let mock_agent = tokio::spawn(async move {
            let mut requests = BufReader::new(agent_input).lines();
            let mut prompt_count = 0;
            let mut resume_count = 0;
            while let Some(line) = requests.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let id = request["id"].clone();
                match request["method"].as_str().unwrap() {
                    "initialize" => {
                        let mut result =
                            serde_json::to_value(InitializeResponse::new(ProtocolVersion::V1))
                                .unwrap();
                        result["agentCapabilities"] = json!({
                            "sessionCapabilities": { "resume": {} }
                        });
                        send_json(
                            &output_tx,
                            json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                        )
                        .await;
                    }
                    "session/new" => {
                        let mcp_servers = &request["params"]["mcpServers"];
                        assert!(
                            mcp_servers.is_null()
                                || mcp_servers.as_array().is_some_and(Vec::is_empty)
                        );
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": NewSessionResponse::new("pi-session"),
                            }),
                        )
                        .await;
                    }
                    "session/resume" => {
                        resume_count += 1;
                        assert_eq!(request["params"]["sessionId"], "pi-session");
                        let mcp_servers = &request["params"]["mcpServers"];
                        assert!(
                            mcp_servers.is_null()
                                || mcp_servers.as_array().is_some_and(Vec::is_empty)
                        );
                        if resume_count == 2 {
                            assert_eq!(
                                request["params"]["additionalDirectories"],
                                json!(["/opt/xpressclaw/presentation-runtime"])
                            );
                        }
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": ResumeSessionResponse::new(),
                            }),
                        )
                        .await;
                    }
                    "session/prompt" => {
                        prompt_count += 1;
                        send_json(
                            &output_tx,
                            json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": PromptResponse::new(StopReason::EndTurn),
                            }),
                        )
                        .await;
                        if prompt_count == 3 {
                            break;
                        }
                    }
                    other => panic!("unexpected ACP method: {other}"),
                }
            }
            assert_eq!(resume_count, 2);
        });

        let process = AcpProcess::start(AttachedContainer {
            info: ContainerInfo {
                container_id: "test-container".to_string(),
                agent_id: "test-project".to_string(),
                status: "running".to_string(),
                host_port: None,
            },
            input: Box::pin(client_input),
            output: Box::pin(ReceiverStream::new(output_rx)),
        })
        .await
        .unwrap();
        let broker = Arc::new(AcpElicitationBroker::new());
        let controls = Arc::new(AcpTurnControlBroker::new());
        let first = process
            .run_turn(
                AcpTurnRuntime::new(first_recorder, broker.clone(), controls.clone()),
                AcpSessionStart::New,
                Path::new("/workspace"),
                "First prompt",
                AcpTurnOptions {
                    mcp_signature: Some("pi-mcp:first".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        process
            .run_turn(
                AcpTurnRuntime::new(second_recorder, broker.clone(), controls.clone()),
                AcpSessionStart::Resume(first.session_id.clone()),
                Path::new("/workspace"),
                "Second prompt",
                AcpTurnOptions {
                    mcp_signature: Some("pi-mcp:second".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        process
            .run_turn(
                AcpTurnRuntime::new(third_recorder, broker, controls),
                AcpSessionStart::Resume(first.session_id),
                Path::new("/workspace"),
                "Third prompt",
                AcpTurnOptions {
                    mcp_signature: Some("pi-mcp:second".into()),
                    additional_directories: vec![PathBuf::from(
                        "/opt/xpressclaw/presentation-runtime",
                    )],
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        mock_agent.await.unwrap();
    }

    #[tokio::test]
    async fn acp_turn_forks_an_inherited_session_when_advertised() {
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
                        let mut result =
                            serde_json::to_value(InitializeResponse::new(ProtocolVersion::V1))
                                .unwrap();
                        result["agentCapabilities"] = json!({
                            "sessionCapabilities": { "fork": {} }
                        });
                        json!({ "jsonrpc": "2.0", "id": id, "result": result })
                    }
                    "session/fork" => {
                        assert_eq!(request["params"]["sessionId"], "source-session");
                        assert_eq!(request["params"]["cwd"], "/workspace");
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": ForkSessionResponse::new("forked-session"),
                        })
                    }
                    "session/prompt" => {
                        assert_eq!(request["params"]["sessionId"], "forked-session");
                        assert_eq!(
                            request["params"]["prompt"][0]["text"],
                            "Continue the old task"
                        );
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

        let result = run_turn(
            AttachedContainer {
                info: ContainerInfo {
                    container_id: "test-container".to_string(),
                    agent_id: "test-attempt".to_string(),
                    status: "running".to_string(),
                    host_port: None,
                },
                input: Box::pin(client_input),
                output: Box::pin(ReceiverStream::new(output_rx)),
            },
            AcpTurnRuntime::new(
                recorder,
                Arc::new(AcpElicitationBroker::new()),
                Arc::new(AcpTurnControlBroker::new()),
            ),
            AcpSessionStart::Fork("source-session".into()),
            Path::new("/workspace"),
            "Continue the old task",
            AcpTurnOptions::default(),
        )
        .await
        .unwrap();

        assert_eq!(result.session_id, "forked-session");
        let events = SessionManager::new(db)
            .list_events("session-1", None, 20)
            .unwrap();
        let fork = events
            .iter()
            .find(|event| event.event_type == "session_fork")
            .unwrap();
        assert_eq!(fork.payload["source_session_id"], "source-session");
        assert_eq!(fork.payload["session_id"], "forked-session");
        assert!(!events
            .iter()
            .any(|event| event.event_type == "session_fork_fallback"));
        mock_agent.await.unwrap();
    }

    #[tokio::test]
    async fn acp_turn_resumes_when_the_agent_cannot_fork() {
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
                        let mut result =
                            serde_json::to_value(InitializeResponse::new(ProtocolVersion::V1))
                                .unwrap();
                        result["agentCapabilities"] = json!({
                            "sessionCapabilities": { "resume": {} }
                        });
                        json!({ "jsonrpc": "2.0", "id": id, "result": result })
                    }
                    "session/resume" => {
                        assert_eq!(request["params"]["sessionId"], "source-session");
                        assert_eq!(request["params"]["cwd"], "/workspace");
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": ResumeSessionResponse::new(),
                        })
                    }
                    "session/prompt" => {
                        assert_eq!(request["params"]["sessionId"], "source-session");
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

        let result = run_turn(
            AttachedContainer {
                info: ContainerInfo {
                    container_id: "test-container".to_string(),
                    agent_id: "test-attempt".to_string(),
                    status: "running".to_string(),
                    host_port: None,
                },
                input: Box::pin(client_input),
                output: Box::pin(ReceiverStream::new(output_rx)),
            },
            AcpTurnRuntime::new(
                recorder,
                Arc::new(AcpElicitationBroker::new()),
                Arc::new(AcpTurnControlBroker::new()),
            ),
            AcpSessionStart::Fork("source-session".into()),
            Path::new("/workspace"),
            "Continue the old task",
            AcpTurnOptions::default(),
        )
        .await
        .unwrap();

        assert_eq!(result.session_id, "source-session");
        let events = SessionManager::new(db)
            .list_events("session-1", None, 20)
            .unwrap();
        let fallback = events
            .iter()
            .find(|event| event.event_type == "session_fork_fallback")
            .unwrap();
        assert_eq!(fallback.payload["source_session_id"], "source-session");
        assert_eq!(fallback.payload["reason"], "unsupported");
        assert!(!events
            .iter()
            .any(|event| event.event_type == "session_fork"));
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
                AcpTurnRuntime::new(recorder, broker, Arc::new(AcpTurnControlBroker::new())),
                AcpSessionStart::New,
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
    fn acp_tool_updates_do_not_prefix_completed_titles() {
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
        assert!(!events
            .iter()
            .any(|event| event.summary == "Completed Run tests"));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.summary == "Run tests")
                .count(),
            2
        );
    }

    #[test]
    fn acp_usage_updates_attempt_state_without_timeline_noise() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db.clone());
        let attempt_id = recorder.attempt_id.clone();
        recorder
            .record_notification(SessionNotification::new(
                "session-1",
                SessionUpdate::UsageUpdate(agent_client_protocol::schema::v1::UsageUpdate::new(
                    125_436, 258_400,
                )),
            ))
            .unwrap();

        let manager = SessionManager::new(db);
        let attempt = manager.get_attempt(&attempt_id).unwrap();
        assert_eq!(attempt.context_used, Some(125_436));
        assert_eq!(attempt.context_size, Some(258_400));
        assert!(!manager
            .list_events("session-1", None, 20)
            .unwrap()
            .iter()
            .any(|event| event.event_type == "usage"));
    }

    #[test]
    fn acp_activity_is_not_capped_at_250_events() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (recorder, _) = test_recorder(db.clone());
        for index in 0..300 {
            recorder
                .record_notification(SessionNotification::new(
                    "session-1",
                    SessionUpdate::ToolCall(
                        ToolCall::new(format!("tool-{index}"), format!("Tool {index}"))
                            .status(ToolCallStatus::InProgress),
                    ),
                ))
                .unwrap();
        }

        let events = SessionManager::new(db)
            .list_events("session-1", None, 500)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "tool_call")
                .count(),
            300
        );
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
        assert!(subtasks.iter().all(|task| task.is_native_plan_item()));
        assert!(subtasks.iter().all(|task| !task.blocks_parent));
    }

    fn test_recorder(db: Arc<Database>) -> (AcpEventRecorder, String) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO agents (id, name, backend, config)
                 VALUES ('session-1', 'Session 1', 'native', '{}')",
                [],
            )
        })
        .unwrap();
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
