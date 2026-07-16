//! Agent Client Protocol transport and event normalization for isolated runners.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, LoadSessionRequest, NewSessionRequest, PermissionOptionKind,
    PlanEntryStatus, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, ResumeSessionRequest, SelectedPermissionOutcome, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOptions,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, TextContent, ToolCallStatus,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use bollard::container::LogOutput;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::db::Database;
use crate::docker::manager::AttachedContainer;
use crate::error::{Error, Result};
use crate::sessions::{NewEvent, SessionManager};
use crate::tasks::board::{ReportedSubtask, TaskBoard, TaskStatus};

const MAX_EVENTS: usize = 250;
const MAX_TRANSCRIPT_UPDATES: usize = 500;
const MAX_DIAGNOSTIC_BYTES: usize = 200_000;

/// Result of one ACP prompt turn. ACP session IDs remain opaque and are only
/// used with `session/resume` or `session/load` on later attempts.
#[derive(Debug)]
pub struct AcpTurnResult {
    pub session_id: String,
    pub summary: String,
    pub stop_reason: String,
    pub diagnostic: String,
}

#[derive(Debug, Default)]
struct TurnState {
    assistant_text: String,
    current_message_id: Option<String>,
    pending_thought: String,
    tool_titles: HashMap<String, String>,
    transcript: Vec<Value>,
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
        state.current_message_id = None;
        state.pending_thought.clear();
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
                    let mut state = self.state.lock().unwrap();
                    if message_id.is_some()
                        && state.current_message_id.is_some()
                        && message_id != state.current_message_id
                        && !state.assistant_text.ends_with('\n')
                    {
                        state.assistant_text.push('\n');
                    }
                    state.current_message_id = message_id.or(state.current_message_id.take());
                    state.assistant_text.push_str(&text.text);
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let ContentBlock::Text(text) = chunk.content {
                    self.state
                        .lock()
                        .unwrap()
                        .pending_thought
                        .push_str(&text.text);
                }
            }
            SessionUpdate::ToolCall(call) => {
                self.flush_thought()?;
                self.state
                    .lock()
                    .unwrap()
                    .tool_titles
                    .insert(call.tool_call_id.to_string(), call.title.clone());
                let summary = tool_summary(&call.title, call.status);
                self.append_event("tool_call", &summary, payload)?;
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.flush_thought()?;
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
                self.flush_thought()?;
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
                self.flush_thought()?;
                self.append_event(
                    "session_mode",
                    &format!("Switched to {} mode", update.current_mode_id),
                    payload,
                )?;
            }
            SessionUpdate::SessionInfoUpdate(update) => {
                self.flush_thought()?;
                if let Some(title) = update.title.take() {
                    self.append_event("session_info", &format!("Session title: {title}"), payload)?;
                }
            }
            SessionUpdate::UsageUpdate(_) => {
                self.flush_thought()?;
                self.append_event("usage", "Updated context usage", payload)?;
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                self.flush_thought()?;
                self.record_config_options(&update.config_options)?;
            }
            SessionUpdate::AvailableCommandsUpdate(_) | SessionUpdate::UserMessageChunk(_) => {}
            _ => {}
        }
        Ok(())
    }

    fn record_permission(&self, request: &RequestPermissionRequest, choice: Option<&str>) {
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

    fn record_model_selection(&self, model: &str) -> Result<()> {
        self.append_event(
            "session_config",
            &format!("Using model {model}"),
            json!({ "category": "model", "value": model }),
        )
    }

    fn record_config_options(&self, options: &[SessionConfigOption]) -> Result<()> {
        let Some((_, choices)) = advertised_model_choices(options) else {
            return Ok(());
        };
        self.append_event(
            "session_config_options",
            &format!("Agent advertised {} models", choices.len()),
            json!({
                "models": choices
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

    fn append_event(&self, event_type: &str, summary: &str, payload: Value) -> Result<()> {
        if self.emitted.fetch_add(1, Ordering::Relaxed) >= MAX_EVENTS {
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
        let summary = state.assistant_text.trim().to_string();
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
    existing_session_id: Option<&str>,
    cwd: &Path,
    prompt: &str,
    model: Option<&str>,
) -> Result<AcpTurnResult> {
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
    let prompt_recorder = recorder.clone();
    let existing_session_id = existing_session_id.map(str::to_owned);
    let cwd = cwd.to_path_buf();
    let prompt = prompt.to_string();
    let model = model.map(str::to_owned);

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
        .connect_with(transport, |connection: ConnectionTo<Agent>| async move {
            let initialized = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;

            let (session_id, config_options) = if let Some(session_id) = existing_session_id {
                if initialized
                    .agent_capabilities
                    .session_capabilities
                    .resume
                    .is_some()
                {
                    let response = connection
                        .send_request(ResumeSessionRequest::new(session_id.clone(), cwd.clone()))
                        .block_task()
                        .await?;
                    (session_id, response.config_options.unwrap_or_default())
                } else if initialized.agent_capabilities.load_session {
                    let response = connection
                        .send_request(LoadSessionRequest::new(session_id.clone(), cwd.clone()))
                        .block_task()
                        .await?;
                    (session_id, response.config_options.unwrap_or_default())
                } else {
                    return Err(agent_client_protocol::util::internal_error(
                        "ACP agent cannot resume or load an existing session",
                    ));
                }
            } else {
                let response = connection
                    .send_request(NewSessionRequest::new(cwd.clone()))
                    .block_task()
                    .await?;
                (
                    response.session_id.to_string(),
                    response.config_options.unwrap_or_default(),
                )
            };

            prompt_recorder
                .record_config_options(&config_options)
                .map_err(agent_client_protocol::Error::into_internal_error)?;

            if let Some(requested_model) = model.as_deref() {
                let (config_id, model_id) =
                    resolve_model_selection(&config_options, requested_model)
                        .map_err(agent_client_protocol::util::internal_error)?;
                connection
                    .send_request(SetSessionConfigOptionRequest::new(
                        session_id.clone(),
                        config_id,
                        model_id.as_str(),
                    ))
                    .block_task()
                    .await?;
                prompt_recorder
                    .record_model_selection(&model_id)
                    .map_err(agent_client_protocol::Error::into_internal_error)?;
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
        ContentChunk, InitializeResponse, NewSessionResponse, Plan, PlanEntry, PlanEntryPriority,
        PromptResponse, SessionConfigSelectOption, SetSessionConfigOptionResponse, StopReason,
        ToolCall, ToolCallUpdate, ToolCallUpdateFields,
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
                    "initialize" => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": InitializeResponse::new(ProtocolVersion::V1),
                    }),
                    "session/new" => json!({
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
                        ]),
                    }),
                    "session/set_config_option" => {
                        assert_eq!(request["params"]["sessionId"], "acp-session-1");
                        assert_eq!(request["params"]["configId"], "model");
                        assert_eq!(request["params"]["value"], "model-test");
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": SetSessionConfigOptionResponse::new(vec![]),
                        })
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
            None,
            Path::new("/workspace"),
            "Do the work",
            Some("Test Model"),
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
        mock_agent.await.unwrap();
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
        (
            AcpEventRecorder::new(db, "session-1", attempt_id, task.id.clone(), "codex"),
            task.id,
        )
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
