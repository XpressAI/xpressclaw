use std::sync::{Arc, RwLock};

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::conversations::event_bus::{ConversationEvent, ConversationEventBus};
use crate::conversations::{ConversationManager, ConversationMessage, SendMessage};
use crate::db::Database;
use crate::error::{Error, Result};
use crate::projects::ensure_project_accepts_work;
use crate::sessions::{NewEvent, SessionManager};
use crate::tasks::board::{CreateTask, Task, TaskBoard, TaskStatus};
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::TaskQueue;
use crate::usage::{resolve_agent_provider, QuotaGateDecision, QuotaGatePolicy, UsageService};

pub const SCHEDULE_TYPE_CRON: &str = "cron";
pub const SCHEDULE_TYPE_ONCE: &str = "once";

const MAX_WAKEUP_DELAY_SECONDS: i64 = 10 * 365 * 24 * 60 * 60;

/// A scheduled task definition stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: String,
    pub name: String,
    pub cron: String,
    pub agent_id: String,
    pub title: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub last_run: Option<String>,
    pub run_count: i64,
    pub created_at: String,
    pub schedule_type: String,
    pub run_at: Option<String>,
    pub continuation_task_id: Option<String>,
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_gate: Option<QuotaGatePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_deferred_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_deferred_reason: Option<String>,
}

/// Result of firing a schedule. Existing task-based schedules serialize as
/// the same Task object as before; Conversation wake-ups report the message
/// that was durably inserted into the Conversation lane.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ScheduleTrigger {
    Task(Task),
    Conversation {
        conversation_id: String,
        agent_id: String,
        message: ConversationMessage,
    },
}

impl ScheduleTrigger {
    pub fn into_task(self) -> Option<Task> {
        match self {
            Self::Task(task) => Some(task),
            Self::Conversation { .. } => None,
        }
    }
}

/// Request to create a new schedule.
#[derive(Debug, Deserialize)]
pub struct CreateSchedule {
    pub name: String,
    pub cron: String,
    pub agent_id: String,
    pub title: String,
    pub description: Option<String>,
    #[serde(default)]
    pub quota_gate: Option<QuotaGatePolicy>,
}

/// Request to create a durable one-shot schedule.
///
/// Exactly one of `run_at` (RFC 3339, including an offset) or
/// `delay_seconds` must be provided. Relative delays are resolved by the
/// control plane so an agent does not need to calculate wall-clock time.
#[derive(Debug, Deserialize)]
pub struct CreateOneShotSchedule {
    pub name: String,
    pub run_at: Option<String>,
    pub delay_seconds: Option<i64>,
    pub agent_id: String,
    pub title: String,
    pub description: Option<String>,
    /// Existing task whose transcript should receive the future turn.
    ///
    /// This is set by the bundled agent control tool. Standalone schedules
    /// leave it empty and retain their existing create-a-task behavior.
    pub continuation_task_id: Option<String>,
    /// Existing Conversation whose Agent lane should receive the future turn.
    ///
    /// The bundled control tool sets this only for Conversation turns. It is
    /// mutually exclusive with `continuation_task_id`.
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub quota_gate: Option<QuotaGatePolicy>,
}

/// Manages recurring and one-shot schedules that create tasks when triggered.
///
/// Handles CRUD for schedule definitions and triggering (creating tasks).
/// The background timer execution is handled by the server layer.
pub struct ScheduleManager {
    db: Arc<Database>,
}

impl ScheduleManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Create a new schedule.
    pub fn create(&self, req: &CreateSchedule) -> Result<Schedule> {
        let quota_gate_json = serialize_quota_gate(req.quota_gate.as_ref())?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_agent_project_accepts_work(&transaction, &req.agent_id)?;
            transaction.execute(
                "INSERT INTO schedules (id, name, cron, agent_id, title, description, enabled, run_count, created_at, quota_gate)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 0, ?7, ?8)",
                rusqlite::params![id, req.name, req.cron, req.agent_id, req.title, req.description, now, quota_gate_json],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;

        self.get(&id)
    }

    /// Create a one-shot schedule that is disabled after its first run.
    pub fn create_one_shot(&self, req: &CreateOneShotSchedule) -> Result<Schedule> {
        validate_required("name", &req.name)?;
        validate_required("agent_id", &req.agent_id)?;
        validate_required("title", &req.title)?;
        let quota_gate_json = serialize_quota_gate(req.quota_gate.as_ref())?;
        if req.continuation_task_id.is_some() && req.conversation_id.is_some() {
            return Err(Error::Schedule(
                "continuation_task_id and conversation_id are mutually exclusive".to_string(),
            ));
        }
        if let Some(task_id) = req.continuation_task_id.as_deref() {
            let task = TaskBoard::new(self.db.clone()).get(task_id).map_err(|_| {
                Error::Schedule(format!("continuation task '{task_id}' was not found"))
            })?;
            validate_continuation_target(&task, &req.agent_id)?;
        }

        let run_at = resolve_one_shot_deadline(req)?.to_rfc3339_opts(SecondsFormat::Secs, true);
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            ensure_agent_project_accepts_work(&transaction, &req.agent_id)?;
            if let Some(conversation_id) = req.conversation_id.as_deref() {
                let is_participant = transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM conversation_participants
                        WHERE conversation_id = ?1
                          AND participant_type = 'agent'
                          AND participant_id = ?2
                    )",
                    rusqlite::params![conversation_id, req.agent_id],
                    |row| row.get::<_, bool>(0),
                )?;
                if !is_participant {
                    return Err(Error::Schedule(format!(
                        "Agent '{}' is not a participant in conversation '{conversation_id}'",
                        req.agent_id
                    )));
                }
            }
            transaction.execute(
                "INSERT INTO schedules
                 (id, name, cron, agent_id, title, description, enabled, run_count, created_at,
                  schedule_type, run_at, continuation_task_id, conversation_id, quota_gate)
                 VALUES (?1, ?2, '', ?3, ?4, ?5, 1, 0, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    id,
                    req.name,
                    req.agent_id,
                    req.title,
                    req.description,
                    now,
                    SCHEDULE_TYPE_ONCE,
                    run_at,
                    req.continuation_task_id,
                    req.conversation_id,
                    quota_gate_json,
                ],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;

        self.get(&id)
    }

    /// Get a schedule by ID.
    pub fn get(&self, id: &str) -> Result<Schedule> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT * FROM schedules WHERE id = ?1")
                .map_err(|e| Error::Database(e.to_string()))?;

            stmt.query_row([id], |row| Ok(row_to_schedule(row)))
                .map_err(|_| Error::ScheduleNotFound { id: id.to_string() })
        })
    }

    /// List all schedules, optionally filtered by agent_id or enabled status.
    pub fn list(&self, agent_id: Option<&str>, enabled_only: bool) -> Result<Vec<Schedule>> {
        self.list_filtered(agent_id, enabled_only, true)
    }

    /// List schedules with optional exclusion of already-fired disabled one-shot wake-ups.
    pub fn list_filtered(
        &self,
        agent_id: Option<&str>,
        enabled_only: bool,
        include_completed_once: bool,
    ) -> Result<Vec<Schedule>> {
        self.db.with_conn(|conn| {
            let mut sql = "SELECT * FROM schedules WHERE 1=1".to_string();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(aid) = agent_id {
                sql.push_str(" AND agent_id = ?");
                params.push(Box::new(aid.to_string()));
            }
            if enabled_only {
                sql.push_str(" AND enabled = 1");
            } else if !include_completed_once {
                sql.push_str(" AND NOT (schedule_type = 'once' AND enabled = 0 AND run_count > 0)");
            }

            sql.push_str(" ORDER BY created_at DESC");

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| Error::Database(e.to_string()))?;
            let schedules = stmt
                .query_map(param_refs.as_slice(), |row| Ok(row_to_schedule(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .collect();

            Ok(schedules)
        })
    }

    /// Delete a schedule.
    pub fn delete(&self, id: &str) -> Result<()> {
        let affected = self.db.with_conn(|conn| {
            conn.execute("DELETE FROM schedules WHERE id = ?1", [id])
                .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::ScheduleNotFound { id: id.to_string() });
        }
        Ok(())
    }

    /// Enable a schedule.
    pub fn enable(&self, id: &str) -> Result<Schedule> {
        self.set_enabled(id, true)
    }

    /// Disable a schedule.
    pub fn disable(&self, id: &str) -> Result<Schedule> {
        self.set_enabled(id, false)
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<Schedule> {
        self.db.with_conn(|conn| {
            // Reserve the write transaction before checking the Project marker
            // so deletion cannot begin between validation and re-enabling.
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let schedule = transaction
                .query_row("SELECT * FROM schedules WHERE id = ?1", [id], |row| {
                    Ok(row_to_schedule(row))
                })
                .map_err(|_| Error::ScheduleNotFound { id: id.to_string() })?;
            if enabled {
                if schedule.schedule_type == SCHEDULE_TYPE_ONCE && schedule.run_count > 0 {
                    return Err(Error::Schedule(
                        "a completed one-shot schedule cannot be enabled again".to_string(),
                    ));
                }
                ensure_agent_project_accepts_work(&transaction, &schedule.agent_id)?;
            }
            transaction.execute(
                "UPDATE schedules
                 SET enabled = ?1,
                     quota_deferred_until = NULL,
                     quota_deferred_reason = NULL
                 WHERE id = ?2",
                rusqlite::params![enabled as i32, id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;
        self.get(id)
    }

    /// Trigger a schedule immediately, creating a task and enqueuing it.
    ///
    /// Supports placeholders in the title:
    /// - `{date}` → current date (YYYY-MM-DD)
    /// - `{time}` → current time (HH:MM)
    /// - `{datetime}` → current datetime
    pub fn trigger(&self, id: &str, board: &TaskBoard) -> Result<ScheduleTrigger> {
        let schedule = self.get(id)?;
        let is_one_shot = schedule.schedule_type == SCHEDULE_TYPE_ONCE;
        if is_one_shot {
            let claimed = self.db.with_conn(|conn| {
                conn.execute(
                    "UPDATE schedules SET enabled = 0
                     WHERE id = ?1 AND enabled = 1 AND run_count = 0",
                    [id],
                )
                .map_err(|e| Error::Database(e.to_string()))
            })?;
            if claimed == 0 {
                return Err(Error::Schedule(
                    "one-shot schedule is disabled or has already run".to_string(),
                ));
            }
        }
        let now = Utc::now().naive_utc();
        let triggered = match self.dispatch_schedule_trigger(&schedule, now, board) {
            Ok(triggered) => triggered,
            Err(error) => {
                if is_one_shot {
                    self.release_one_shot_claim(id);
                }
                return Err(error);
            }
        };

        // Update last_run and run_count
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules
                 SET last_run = ?1,
                     run_count = run_count + 1,
                     quota_deferred_until = NULL,
                     quota_deferred_reason = NULL,
                     enabled = CASE WHEN schedule_type = 'once' THEN 0 ELSE enabled END
                 WHERE id = ?2",
                rusqlite::params![now_str, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(triggered)
    }

    /// Atomically claim and dispatch an automatic scheduled occurrence.
    ///
    /// Returns `Ok(None)` if the schedule was disabled, deleted, or already triggered/skipped
    /// since `expected` was loaded (e.g., while an asynchronous quota check was in flight).
    pub fn trigger_scheduled(
        &self,
        expected: &Schedule,
        board: &TaskBoard,
    ) -> Result<Option<ScheduleTrigger>> {
        let now = Utc::now().naive_utc();
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let claimed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules
                 SET last_run = ?1,
                     run_count = run_count + 1,
                     quota_deferred_until = NULL,
                     quota_deferred_reason = NULL,
                     enabled = CASE WHEN schedule_type = 'once' THEN 0 ELSE enabled END
                 WHERE id = ?2
                   AND enabled = 1
                   AND run_count = ?3
                   AND last_run IS ?4",
                rusqlite::params![now_str, expected.id, expected.run_count, expected.last_run],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        if claimed == 0 {
            return Ok(None);
        }

        match self.dispatch_schedule_trigger(expected, now, board) {
            Ok(triggered) => Ok(Some(triggered)),
            Err(error) => {
                self.release_scheduled_claim(expected, &now_str);
                Err(error)
            }
        }
    }

    fn dispatch_schedule_trigger(
        &self,
        schedule: &Schedule,
        now: chrono::NaiveDateTime,
        board: &TaskBoard,
    ) -> Result<ScheduleTrigger> {
        let id = schedule.id.as_str();
        let is_one_shot = schedule.schedule_type == SCHEDULE_TYPE_ONCE;

        // Format title with date/time placeholders
        let title = schedule
            .title
            .replace("{date}", &now.format("%Y-%m-%d").to_string())
            .replace("{time}", &now.format("%H:%M").to_string())
            .replace("{datetime}", &now.format("%Y-%m-%d %H:%M").to_string());

        let description = schedule.description.as_ref().map(|d| {
            d.replace("{date}", &now.format("%Y-%m-%d").to_string())
                .replace("{time}", &now.format("%H:%M").to_string())
                .replace("{datetime}", &now.format("%Y-%m-%d %H:%M").to_string())
        });

        let agent_id = schedule.agent_id.clone();
        if let Some(task_id) = schedule.continuation_task_id.as_deref() {
            let task = self.enqueue_continuation(
                schedule,
                task_id,
                &title,
                description.as_deref(),
                board,
            )?;
            Ok(ScheduleTrigger::Task(task))
        } else if let Some(conversation_id) = schedule.conversation_id.as_deref() {
            let message = self.enqueue_conversation_wakeup(
                schedule,
                conversation_id,
                &title,
                description.as_deref(),
            )?;
            Ok(ScheduleTrigger::Conversation {
                conversation_id: conversation_id.to_string(),
                agent_id,
                message,
            })
        } else {
            let task = board.create(&CreateTask {
                backlog: false,
                start_after: None,
                title,
                description,
                agent_id: Some(agent_id.clone()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: Some(serde_json::json!({
                    "origin": "schedule",
                    "kind": "scheduled",
                    "source_id": id,
                    "schedule_type": schedule.schedule_type,
                })),
            })?;

            // Standalone and recurring schedules retain their existing
            // create-a-task behavior.
            let queue = TaskQueue::new(self.db.clone());
            if let Err(error) = queue.enqueue(&task.id, &agent_id) {
                if is_one_shot {
                    let _ = board.delete(&task.id);
                    return Err(error);
                }
                warn!(
                    task_id = task.id.as_str(),
                    schedule_id = id,
                    error = %error,
                    "failed to enqueue scheduled task"
                );
            }
            Ok(ScheduleTrigger::Task(task))
        }
    }

    fn release_scheduled_claim(&self, expected: &Schedule, claimed_at: &str) {
        let _ = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules
                 SET last_run = ?1,
                     run_count = ?2,
                     quota_deferred_until = ?3,
                     quota_deferred_reason = ?4,
                     enabled = CASE WHEN schedule_type = 'once' THEN 1 ELSE enabled END
                 WHERE id = ?5
                   AND run_count = ?6
                   AND last_run = ?7",
                rusqlite::params![
                    expected.last_run,
                    expected.run_count,
                    expected.quota_deferred_until,
                    expected.quota_deferred_reason,
                    expected.id,
                    expected.run_count + 1,
                    claimed_at,
                ],
            )
        });
    }

    pub fn record_quota_deferral(
        &self,
        schedule: &Schedule,
        resume_at: &str,
        reason: &str,
    ) -> Result<bool> {
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules
                 SET quota_deferred_until = ?1,
                     quota_deferred_reason = ?2
                 WHERE id = ?3
                   AND enabled = 1
                   AND run_count = ?4
                   AND last_run IS ?5",
                rusqlite::params![
                    resume_at,
                    reason,
                    schedule.id,
                    schedule.run_count,
                    schedule.last_run
                ],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        Ok(changed == 1)
    }

    pub fn clear_quota_deferral(&self, schedule: &Schedule) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules
                 SET quota_deferred_until = NULL,
                     quota_deferred_reason = NULL
                 WHERE id = ?1
                   AND run_count = ?2
                   AND last_run IS ?3",
                rusqlite::params![schedule.id, schedule.run_count, schedule.last_run],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        Ok(())
    }

    pub fn skip_scheduled_run(&self, schedule: &Schedule, now: DateTime<Utc>) -> Result<()> {
        let now_str = now.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules
                 SET last_run = ?1,
                     quota_deferred_until = NULL,
                     quota_deferred_reason = NULL,
                     enabled = CASE WHEN schedule_type = 'once' THEN 0 ELSE enabled END
                 WHERE id = ?2
                   AND enabled = 1
                   AND run_count = ?3
                   AND last_run IS ?4",
                rusqlite::params![now_str, schedule.id, schedule.run_count, schedule.last_run],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;
        Ok(())
    }

    fn enqueue_continuation(
        &self,
        schedule: &Schedule,
        task_id: &str,
        title: &str,
        description: Option<&str>,
        board: &TaskBoard,
    ) -> Result<Task> {
        let task = board
            .get(task_id)
            .map_err(|_| Error::Schedule(format!("continuation task '{task_id}' was not found")))?;
        validate_continuation_target(&task, &schedule.agent_id)?;

        let instruction = description
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(title);
        let content = format!("Scheduled wake-up: {}\n\n{instruction}", schedule.name);
        let conversation = TaskConversation::new(self.db.clone());
        let message = conversation.add_message(task_id, "user", &content)?;
        let queue = TaskQueue::new(self.db.clone());
        if let Err(error) = queue.enqueue_continuation_for_message(
            task_id,
            &schedule.agent_id,
            message.id,
            &message.timestamp,
        ) {
            let _ = self.db.with_conn(|conn| {
                conn.execute("DELETE FROM task_messages WHERE id = ?1", [message.id])
            });
            return Err(error);
        }

        if let Err(error) = SessionManager::new(self.db.clone()).append_event(
            &schedule.agent_id,
            NewEvent {
                attempt_id: None,
                task_id: Some(task_id),
                source_type: "schedule",
                source_id: Some(&schedule.id),
                event_type: "scheduled_wakeup",
                summary: &format!("Scheduled wake-up '{}' fired", schedule.name),
                payload: serde_json::json!({
                    "kind": "scheduled_wakeup",
                    "schedule_id": schedule.id,
                    "content": instruction,
                }),
            },
        ) {
            warn!(
                task_id,
                schedule_id = schedule.id,
                error = %error,
                "failed to record scheduled wake-up event"
            );
        }

        if matches!(
            task.status,
            TaskStatus::WaitingForInput
                | TaskStatus::Blocked
                | TaskStatus::Completed
                | TaskStatus::Cancelled
        ) {
            if let Err(error) = board.update_status(task_id, "pending", Some(&schedule.agent_id)) {
                warn!(
                    task_id,
                    schedule_id = schedule.id,
                    error = %error,
                    "failed to reopen task for scheduled wake-up"
                );
            }
        }

        board.get(task_id)
    }

    fn enqueue_conversation_wakeup(
        &self,
        schedule: &Schedule,
        conversation_id: &str,
        title: &str,
        description: Option<&str>,
    ) -> Result<ConversationMessage> {
        let instruction = description
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(title);
        let content = format!("Scheduled wake-up: {}\n\n{}", schedule.name, instruction);
        let manager = ConversationManager::new(self.db.clone());
        let (message, _, _) = manager.send_targeted_routed_message_with_attachments(
            conversation_id,
            &schedule.agent_id,
            &SendMessage {
                sender_type: "system".to_string(),
                sender_id: "xpressclaw-scheduler".to_string(),
                sender_name: Some("Scheduled wake-up".to_string()),
                content,
                message_type: Some("scheduled_wakeup".to_string()),
            },
            Some(&serde_json::json!({
                "schedule_id": schedule.id,
                "target_agent_id": schedule.agent_id,
            })),
            &[],
        )?;
        Ok(message)
    }

    fn release_one_shot_claim(&self, id: &str) {
        let _ = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules SET enabled = 1 WHERE id = ?1 AND run_count = 0",
                [id],
            )
        });
    }
}

fn ensure_agent_project_accepts_work(conn: &rusqlite::Connection, agent_id: &str) -> Result<()> {
    let project_id = conn
        .query_row(
            "SELECT project_id FROM agents WHERE id = ?1",
            [agent_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .ok_or_else(|| Error::AgentNotFound {
            name: agent_id.to_string(),
        })?;
    if let Some(project_id) = project_id.as_deref() {
        ensure_project_accepts_work(conn, project_id)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Background cron runner
// ---------------------------------------------------------------------------

/// Start the schedule runner background loop.
///
/// Checks all enabled schedules every 60 seconds and triggers due one-shot
/// deadlines or cron expressions that match the current time.
pub async fn start_schedule_runner(db: Arc<Database>) {
    start_schedule_runner_with_events(db, None).await;
}

pub async fn start_schedule_runner_with_events(
    db: Arc<Database>,
    event_bus: Option<Arc<ConversationEventBus>>,
) {
    start_schedule_runner_with_usage(db, event_bus, None, None).await;
}

pub async fn start_schedule_runner_with_usage(
    db: Arc<Database>,
    event_bus: Option<Arc<ConversationEventBus>>,
    config: Option<Arc<RwLock<Arc<Config>>>>,
    usage: Option<Arc<UsageService>>,
) {
    info!("schedule runner started");

    loop {
        let cfg_snapshot = config
            .as_ref()
            .and_then(|c| c.read().ok().map(|guard| guard.clone()));
        if let Err(e) = check_schedules_with_usage(
            &db,
            event_bus.as_deref(),
            cfg_snapshot.as_deref(),
            usage.as_deref(),
            Utc::now(),
        )
        .await
        {
            error!(error = %e, "schedule check error");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

#[cfg(test)]
fn check_schedules(db: &Arc<Database>, event_bus: Option<&ConversationEventBus>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Backend(e.to_string()))?;
    rt.block_on(check_schedules_with_usage(
        db,
        event_bus,
        None,
        None,
        Utc::now(),
    ))
}

pub async fn check_schedules_with_usage(
    db: &Arc<Database>,
    event_bus: Option<&ConversationEventBus>,
    config: Option<&Config>,
    usage: Option<&UsageService>,
    now: DateTime<Utc>,
) -> Result<()> {
    let mgr = ScheduleManager::new(db.clone());
    let board = TaskBoard::new(db.clone());

    let schedules = mgr.list(None, true)?; // enabled only
    if schedules.is_empty() {
        return Ok(());
    }

    for schedule in &schedules {
        let deferred_until = schedule
            .quota_deferred_until
            .as_deref()
            .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|dt| dt.with_timezone(&Utc));

        let deferred_due = match deferred_until {
            Some(until) if until > now => continue,
            Some(_) => schedule.schedule_type != SCHEDULE_TYPE_ONCE || schedule.run_count == 0,
            None => false,
        };

        if deferred_due || should_trigger(schedule, now) {
            if let (Some(gate), Some(usage_service)) = (schedule.quota_gate.as_ref(), usage) {
                let provider = gate
                    .provider
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|p| {
                        crate::acp::canonical_agent_kind(p)
                            .unwrap_or(p)
                            .to_ascii_lowercase()
                    })
                    .or_else(|| resolve_agent_provider(db, config, &schedule.agent_id));
                if let Some(provider) = provider {
                    match usage_service
                        .evaluate_quota_gate_with_refresh(&provider, gate, now, deferred_due)
                        .await
                    {
                        QuotaGateDecision::Allow => {}
                        QuotaGateDecision::Defer { until, reason } => {
                            let resume_at = until.to_rfc3339_opts(SecondsFormat::Secs, true);
                            if mgr.record_quota_deferral(schedule, &resume_at, &reason)? {
                                info!(
                                    schedule_id = schedule.id.as_str(),
                                    provider = %provider,
                                    resume_at = %resume_at,
                                    "deferred scheduled task until quota reset"
                                );
                            } else {
                                debug!(
                                    schedule_id = schedule.id.as_str(),
                                    "skipped persisting quota deferral because schedule changed"
                                );
                            }
                            continue;
                        }
                        QuotaGateDecision::Skip { reason } => {
                            mgr.skip_scheduled_run(schedule, now)?;
                            info!(
                                schedule_id = schedule.id.as_str(),
                                provider = %provider,
                                reason = %reason,
                                "skipped scheduled task due to quota gate"
                            );
                            continue;
                        }
                    }
                }
            }

            info!(
                schedule_id = schedule.id.as_str(),
                name = schedule.name.as_str(),
                agent_id = schedule.agent_id.as_str(),
                "triggering scheduled task"
            );
            match mgr.trigger_scheduled(schedule, &board) {
                Ok(Some(ScheduleTrigger::Task(task))) => info!(
                    schedule_id = schedule.id.as_str(),
                    task_id = task.id.as_str(),
                    "scheduled task created"
                ),
                Ok(Some(ScheduleTrigger::Conversation {
                    conversation_id,
                    agent_id,
                    message,
                })) => {
                    if let Some(bus) = event_bus {
                        if let Ok(value) = serde_json::to_value(&message) {
                            bus.send(
                                &conversation_id,
                                ConversationEvent::Message { message: value },
                            );
                        }
                    }
                    info!(
                        schedule_id = schedule.id.as_str(),
                        conversation_id,
                        agent_id,
                        message_id = message.id,
                        "scheduled Conversation turn created"
                    );
                }
                Ok(None) => {
                    debug!(
                        schedule_id = schedule.id.as_str(),
                        "skipped triggering schedule because it was disabled or already claimed"
                    );
                }
                Err(e) => {
                    error!(
                        schedule_id = schedule.id.as_str(),
                        error = %e,
                        "failed to trigger schedule"
                    );
                }
            }
        }
    }

    Ok(())
}

/// Parse a cron expression using croner.
/// Supports both 5-field (standard) and 6-field (with seconds) expressions.
fn parse_cron(expr: &str) -> std::result::Result<croner::Cron, croner::errors::CronError> {
    croner::Cron::new(expr).parse()
}

/// Check if a schedule should trigger now based on its cron expression.
///
/// Cron expressions are evaluated against the server's local time so that
/// "0 9 * * *" means 9am in the user's timezone, not 9am UTC.
/// Checks if a cron match occurred between last_run and now.
fn should_trigger(schedule: &Schedule, now: chrono::DateTime<Utc>) -> bool {
    if schedule.schedule_type == SCHEDULE_TYPE_ONCE {
        return schedule.run_count == 0
            && schedule
                .run_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|run_at| run_at.with_timezone(&Utc) <= now);
    }

    // Convert to local time for cron matching
    let now_local = now.with_timezone(&chrono::Local);
    let cron = match parse_cron(&schedule.cron) {
        Ok(c) => c,
        Err(e) => {
            debug!(
                schedule_id = schedule.id.as_str(),
                cron = schedule.cron.as_str(),
                error = %e,
                "invalid cron expression, skipping"
            );
            return false;
        }
    };

    let check_from = schedule
        .last_run
        .as_deref()
        .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok())
        .map(|dt| dt.and_utc().with_timezone(&chrono::Local))
        .unwrap_or_else(|| now_local - chrono::Duration::minutes(2));

    // Find the next cron match after last_run (in local time).
    // If it's <= now, we should trigger.
    let mut iter = cron.iter_after(check_from);
    match iter.next() {
        Some(next) => next <= now_local,
        None => false,
    }
}

fn serialize_quota_gate(gate: Option<&QuotaGatePolicy>) -> Result<Option<String>> {
    let Some(gate) = gate else {
        return Ok(None);
    };
    gate.validate().map_err(Error::Schedule)?;
    serde_json::to_string(gate)
        .map(Some)
        .map_err(|e| Error::Schedule(format!("invalid quota_gate: {e}")))
}

fn row_to_schedule(row: &rusqlite::Row) -> Schedule {
    let quota_gate = row
        .get::<_, Option<String>>("quota_gate")
        .unwrap_or_default()
        .and_then(|raw| serde_json::from_str::<QuotaGatePolicy>(&raw).ok());
    Schedule {
        id: row.get("id").unwrap_or_default(),
        name: row.get("name").unwrap_or_default(),
        cron: row.get("cron").unwrap_or_default(),
        agent_id: row.get("agent_id").unwrap_or_default(),
        title: row.get("title").unwrap_or_default(),
        description: row.get("description").unwrap_or_default(),
        enabled: row.get::<_, i32>("enabled").unwrap_or(1) != 0,
        last_run: row.get("last_run").unwrap_or_default(),
        run_count: row.get("run_count").unwrap_or(0),
        created_at: row.get("created_at").unwrap_or_default(),
        schedule_type: row
            .get("schedule_type")
            .unwrap_or_else(|_| SCHEDULE_TYPE_CRON.to_string()),
        run_at: row.get("run_at").unwrap_or_default(),
        continuation_task_id: row.get("continuation_task_id").unwrap_or_default(),
        conversation_id: row.get("conversation_id").unwrap_or_default(),
        quota_gate,
        quota_deferred_until: row.get("quota_deferred_until").unwrap_or_default(),
        quota_deferred_reason: row.get("quota_deferred_reason").unwrap_or_default(),
    }
}

fn validate_required(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::Schedule(format!("{name} cannot be empty")));
    }
    Ok(())
}

fn validate_continuation_target(task: &Task, agent_id: &str) -> Result<()> {
    if task.agent_id.as_deref() != Some(agent_id) {
        return Err(Error::Schedule(format!(
            "continuation task '{}' does not belong to project '{agent_id}'",
            task.id
        )));
    }
    if task.hidden || task.task_type == "IDLE" {
        return Err(Error::Schedule(format!(
            "continuation task '{}' is hidden or idle",
            task.id
        )));
    }
    Ok(())
}

fn resolve_one_shot_deadline(req: &CreateOneShotSchedule) -> Result<DateTime<Utc>> {
    match (req.run_at.as_deref(), req.delay_seconds) {
        (Some(run_at), None) => DateTime::parse_from_rfc3339(run_at.trim())
            .map(|value| value.with_timezone(&Utc))
            .map_err(|_| {
                Error::Schedule(
                    "run_at must be an RFC 3339 timestamp with a timezone offset".to_string(),
                )
            }),
        (None, Some(delay)) if (1..=MAX_WAKEUP_DELAY_SECONDS).contains(&delay) => Utc::now()
            .checked_add_signed(chrono::Duration::seconds(delay))
            .ok_or_else(|| Error::Schedule("delay_seconds is out of range".to_string())),
        (None, Some(_)) => Err(Error::Schedule(format!(
            "delay_seconds must be between 1 and {MAX_WAKEUP_DELAY_SECONDS}"
        ))),
        (Some(_), Some(_)) => Err(Error::Schedule(
            "provide exactly one of run_at or delay_seconds".to_string(),
        )),
        (None, None) => Err(Error::Schedule(
            "provide exactly one of run_at or delay_seconds".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, ScheduleManager, TaskBoard) {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO agents (id, name, backend, config)
                 VALUES ('atlas', 'Atlas', 'native', '{}'),
                        ('scout', 'Scout', 'native', '{}');",
            )
        })
        .unwrap();
        let mgr = ScheduleManager::new(db.clone());
        let board = TaskBoard::new(db.clone());
        (db, mgr, board)
    }

    fn create_schedule(mgr: &ScheduleManager) -> Schedule {
        mgr.create(&CreateSchedule {
            name: "Daily standup".to_string(),
            cron: "0 9 * * *".to_string(),
            agent_id: "atlas".to_string(),
            title: "Daily standup {date}".to_string(),
            description: Some("Run the daily standup report".to_string()),
            quota_gate: None,
        })
        .unwrap()
    }

    fn create_one_shot(mgr: &ScheduleManager, delay_seconds: i64) -> Schedule {
        mgr.create_one_shot(&CreateOneShotSchedule {
            name: "Experiment wake-up".to_string(),
            run_at: None,
            delay_seconds: Some(delay_seconds),
            agent_id: "atlas".to_string(),
            title: "Resume experiment".to_string(),
            description: Some("Inspect the completed experiment and continue the goal.".into()),
            continuation_task_id: None,
            conversation_id: None,
            quota_gate: None,
        })
        .unwrap()
    }

    #[test]
    fn test_create_and_get() {
        let (_, mgr, _) = setup();
        let schedule = create_schedule(&mgr);

        assert_eq!(schedule.name, "Daily standup");
        assert_eq!(schedule.cron, "0 9 * * *");
        assert_eq!(schedule.agent_id, "atlas");
        assert!(schedule.enabled);
        assert_eq!(schedule.run_count, 0);

        let fetched = mgr.get(&schedule.id).unwrap();
        assert_eq!(fetched.id, schedule.id);
        assert_eq!(fetched.name, "Daily standup");
    }

    #[test]
    fn test_create_one_shot_with_relative_delay() {
        let (_, mgr, _) = setup();
        let before = Utc::now();
        let schedule = create_one_shot(&mgr, 5 * 60 * 60);
        let deadline = DateTime::parse_from_rfc3339(schedule.run_at.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);

        assert_eq!(schedule.schedule_type, SCHEDULE_TYPE_ONCE);
        assert!(schedule.cron.is_empty());
        assert!(schedule.enabled);
        assert!(deadline >= before + chrono::Duration::seconds(5 * 60 * 60 - 1));
        assert!(deadline <= before + chrono::Duration::seconds(5 * 60 * 60 + 1));
    }

    #[test]
    fn test_one_shot_requires_exactly_one_deadline() {
        let (_, mgr, _) = setup();
        let invalid = CreateOneShotSchedule {
            name: "Invalid".into(),
            run_at: Some(Utc::now().to_rfc3339()),
            delay_seconds: Some(60),
            agent_id: "atlas".into(),
            title: "Invalid".into(),
            description: None,
            continuation_task_id: None,
            conversation_id: None,
            quota_gate: None,
        };

        assert!(matches!(
            mgr.create_one_shot(&invalid),
            Err(Error::Schedule(_))
        ));
    }

    #[test]
    fn test_list() {
        let (_, mgr, _) = setup();
        create_schedule(&mgr);

        mgr.create(&CreateSchedule {
            name: "Weekly report".to_string(),
            cron: "0 10 * * 1".to_string(),
            agent_id: "scout".to_string(),
            title: "Weekly report".to_string(),
            description: None,
            quota_gate: None,
        })
        .unwrap();

        let all = mgr.list(None, false).unwrap();
        assert_eq!(all.len(), 2);

        let atlas_only = mgr.list(Some("atlas"), false).unwrap();
        assert_eq!(atlas_only.len(), 1);
        assert_eq!(atlas_only[0].agent_id, "atlas");
    }

    #[test]
    fn test_delete() {
        let (_, mgr, _) = setup();
        let schedule = create_schedule(&mgr);

        mgr.delete(&schedule.id).unwrap();
        assert!(mgr.get(&schedule.id).is_err());
    }

    #[test]
    fn test_delete_not_found() {
        let (_, mgr, _) = setup();
        let result = mgr.delete("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_enable_disable() {
        let (_, mgr, _) = setup();
        let schedule = create_schedule(&mgr);

        let disabled = mgr.disable(&schedule.id).unwrap();
        assert!(!disabled.enabled);

        let enabled = mgr.enable(&schedule.id).unwrap();
        assert!(enabled.enabled);
    }

    #[test]
    fn deleting_project_cannot_reenable_its_schedule() {
        let (db, mgr, _) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One');
                 UPDATE agents SET project_id = 'project-one' WHERE id = 'atlas';",
            )
        })
        .unwrap();
        let schedule = create_schedule(&mgr);
        mgr.disable(&schedule.id).unwrap();

        crate::projects::ProjectManager::new(db)
            .begin_cascade("project-one")
            .unwrap();

        let error = mgr.enable(&schedule.id).unwrap_err();
        assert!(error.to_string().contains("being deleted"));
        assert!(!mgr.get(&schedule.id).unwrap().enabled);
    }

    #[test]
    fn deleted_agents_cannot_receive_new_schedules() {
        let (db, mgr, _) = setup();
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One');
                 UPDATE agents SET project_id = 'project-one' WHERE id = 'atlas';",
            )
        })
        .unwrap();
        let projects = crate::projects::ProjectManager::new(db.clone());
        projects.begin_cascade("project-one").unwrap();
        projects.finish_cascade("project-one").unwrap();

        let recurring_error = mgr
            .create(&CreateSchedule {
                name: "Stale recurring work".into(),
                cron: "0 9 * * *".into(),
                agent_id: "atlas".into(),
                title: "Do stale work".into(),
                description: None,
                quota_gate: None,
            })
            .unwrap_err();
        assert!(matches!(
            recurring_error,
            Error::AgentNotFound { ref name } if name == "atlas"
        ));

        let one_shot_error = mgr
            .create_one_shot(&CreateOneShotSchedule {
                name: "Stale wake-up".into(),
                run_at: None,
                delay_seconds: Some(60),
                agent_id: "atlas".into(),
                title: "Resume stale work".into(),
                description: None,
                continuation_task_id: None,
                conversation_id: None,
                quota_gate: None,
            })
            .unwrap_err();
        assert!(matches!(
            one_shot_error,
            Error::AgentNotFound { ref name } if name == "atlas"
        ));
        assert!(mgr.list(None, false).unwrap().is_empty());
    }

    #[test]
    fn test_trigger_creates_task() {
        let (_, mgr, board) = setup();
        let schedule = create_schedule(&mgr);

        let task = mgr
            .trigger(&schedule.id, &board)
            .unwrap()
            .into_task()
            .unwrap();

        // Title should have date placeholder replaced
        assert!(!task.title.contains("{date}"));
        assert!(task.title.starts_with("Daily standup "));
        assert_eq!(task.agent_id.as_deref(), Some("atlas"));

        // Schedule should be updated
        let updated = mgr.get(&schedule.id).unwrap();
        assert_eq!(updated.run_count, 1);
        assert!(updated.last_run.is_some());
    }

    #[test]
    fn test_trigger_multiple_times() {
        let (_, mgr, board) = setup();
        let schedule = create_schedule(&mgr);

        mgr.trigger(&schedule.id, &board).unwrap();
        mgr.trigger(&schedule.id, &board).unwrap();
        mgr.trigger(&schedule.id, &board).unwrap();

        let updated = mgr.get(&schedule.id).unwrap();
        assert_eq!(updated.run_count, 3);
    }

    #[test]
    fn test_one_shot_triggers_only_once_and_disables_itself() {
        let (db, mgr, board) = setup();
        let schedule = create_one_shot(&mgr, 60);

        let task = mgr
            .trigger(&schedule.id, &board)
            .unwrap()
            .into_task()
            .unwrap();
        assert_eq!(
            task.context
                .as_ref()
                .and_then(|value| value.get("schedule_type"))
                .and_then(|value| value.as_str()),
            Some(SCHEDULE_TYPE_ONCE)
        );
        let updated = mgr.get(&schedule.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.run_count, 1);
        assert!(matches!(
            mgr.trigger(&schedule.id, &board),
            Err(Error::Schedule(_))
        ));
        assert!(matches!(mgr.enable(&schedule.id), Err(Error::Schedule(_))));
        assert_eq!(TaskQueue::new(db).pending_count("atlas").unwrap(), 1);
    }

    #[test]
    fn test_one_shot_continues_the_task_that_armed_it() {
        let (db, mgr, board) = setup();
        let original = board
            .create(&CreateTask {
                title: "Monitor the deployment".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();
        board
            .update_status(&original.id, "completed", Some("atlas"))
            .unwrap();
        let schedule = mgr
            .create_one_shot(&CreateOneShotSchedule {
                name: "Check deployment".into(),
                run_at: None,
                delay_seconds: Some(60),
                agent_id: "atlas".into(),
                title: "Check deployment".into(),
                description: Some("Inspect CI and report whether it passed.".into()),
                continuation_task_id: Some(original.id.clone()),
                conversation_id: None,
                quota_gate: None,
            })
            .unwrap();

        let triggered = mgr
            .trigger(&schedule.id, &board)
            .unwrap()
            .into_task()
            .unwrap();

        assert_eq!(triggered.id, original.id);
        assert_eq!(triggered.status, TaskStatus::Pending);
        assert_eq!(board.list(None, Some("atlas"), 10).unwrap().len(), 1);
        let messages = TaskConversation::new(db.clone())
            .get_messages(&original.id)
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "user");
        assert_eq!(
            messages[0].content,
            "Scheduled wake-up: Check deployment\n\nInspect CI and report whether it passed."
        );
        let queued = TaskQueue::new(db.clone())
            .list(Some("atlas"), Some("queued"), 10)
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].task_id, original.id);
        let events = SessionManager::new(db)
            .list_events("atlas", None, 100)
            .unwrap();
        assert!(events.iter().any(|event| {
            event.task_id.as_deref() == Some(original.id.as_str())
                && event.event_type == "scheduled_wakeup"
                && event.source_id.as_deref() == Some(schedule.id.as_str())
        }));
    }

    #[test]
    fn test_one_shot_returns_to_the_conversation_agent_lane() {
        let (db, mgr, board) = setup();
        crate::agents::registry::AgentRegistry::new(db.clone())
            .ensure("atlas", "generic")
            .unwrap();
        let conversations = ConversationManager::new(db.clone());
        let conversation = conversations
            .create(&crate::conversations::CreateConversation {
                title: Some("Release room".into()),
                icon: None,
                participant_ids: vec!["atlas".into()],
            })
            .unwrap();
        let schedule = mgr
            .create_one_shot(&CreateOneShotSchedule {
                name: "Check release".into(),
                run_at: None,
                delay_seconds: Some(60),
                agent_id: "atlas".into(),
                title: "Check release".into(),
                description: Some("Inspect CI and update the room.".into()),
                continuation_task_id: None,
                conversation_id: Some(conversation.id.clone()),
                quota_gate: None,
            })
            .unwrap();

        let triggered = mgr.trigger(&schedule.id, &board).unwrap();
        let ScheduleTrigger::Conversation {
            conversation_id,
            agent_id,
            message,
        } = triggered
        else {
            panic!("expected a Conversation wake-up");
        };

        assert_eq!(conversation_id, conversation.id);
        assert_eq!(agent_id, "atlas");
        assert_eq!(message.message_type, "scheduled_wakeup");
        assert!(message.content.contains("Inspect CI and update the room."));
        assert!(board.list(None, Some("atlas"), 10).unwrap().is_empty());
        let turns = crate::conversations::runtime::ConversationTurnQueue::new(db)
            .list_for_conversation(&conversation.id, 10)
            .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].agent_id, "atlas");
        assert_eq!(turns[0].trigger_message_id, Some(message.id));
    }

    #[test]
    fn test_one_shot_rejects_a_continuation_task_from_another_project() {
        let (_, mgr, board) = setup();
        let task = board
            .create(&CreateTask {
                title: "Atlas task".into(),
                agent_id: Some("atlas".into()),
                ..Default::default()
            })
            .unwrap();

        let result = mgr.create_one_shot(&CreateOneShotSchedule {
            name: "Cross-project wake-up".into(),
            run_at: None,
            delay_seconds: Some(60),
            agent_id: "scout".into(),
            title: "Wrong project".into(),
            description: Some("This must be rejected.".into()),
            continuation_task_id: Some(task.id),
            conversation_id: None,
            quota_gate: None,
        });

        assert!(matches!(result, Err(Error::Schedule(_))));
        assert!(mgr.list(None, false).unwrap().is_empty());
    }

    #[test]
    fn test_one_shot_rejects_a_hidden_idle_continuation_task() {
        let (_, mgr, board) = setup();
        let task = board
            .create_idle_task("atlas", "Check whether there is proactive work")
            .unwrap();

        let result = mgr.create_one_shot(&CreateOneShotSchedule {
            name: "Idle wake-up".into(),
            run_at: None,
            delay_seconds: Some(60),
            agent_id: "atlas".into(),
            title: "Resume visible work".into(),
            description: Some("This must become a standalone task instead.".into()),
            continuation_task_id: Some(task.id),
            conversation_id: None,
            quota_gate: None,
        });

        assert!(matches!(
            result,
            Err(Error::Schedule(message)) if message.contains("hidden or idle")
        ));
        assert!(mgr.list(None, false).unwrap().is_empty());
    }

    #[test]
    fn test_due_one_shot_is_recovered_and_dispatched_once() {
        let (db, mgr, _) = setup();
        let schedule = mgr
            .create_one_shot(&CreateOneShotSchedule {
                name: "Overdue wake-up".into(),
                run_at: Some(
                    (Utc::now() - chrono::Duration::minutes(5))
                        .to_rfc3339_opts(SecondsFormat::Secs, true),
                ),
                delay_seconds: None,
                agent_id: "atlas".into(),
                title: "Resume overdue work".into(),
                description: None,
                continuation_task_id: None,
                conversation_id: None,
                quota_gate: None,
            })
            .unwrap();

        check_schedules(&db, None).unwrap();
        check_schedules(&db, None).unwrap();

        let updated = mgr.get(&schedule.id).unwrap();
        assert!(!updated.enabled);
        assert_eq!(updated.run_count, 1);
        assert_eq!(TaskQueue::new(db).pending_count("atlas").unwrap(), 1);
    }

    #[test]
    fn test_list_enabled_only() {
        let (_, mgr, _) = setup();
        let s1 = create_schedule(&mgr);

        mgr.create(&CreateSchedule {
            name: "Disabled one".to_string(),
            cron: "0 0 * * *".to_string(),
            agent_id: "atlas".to_string(),
            title: "Disabled".to_string(),
            description: None,
            quota_gate: None,
        })
        .unwrap();

        // Disable the second schedule
        let all = mgr.list(None, false).unwrap();
        let s2 = all.iter().find(|s| s.name == "Disabled one").unwrap();
        mgr.disable(&s2.id).unwrap();

        let enabled = mgr.list(None, true).unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, s1.id);
    }

    #[test]
    fn test_get_not_found() {
        let (_, mgr, _) = setup();
        let result = mgr.get("nonexistent");
        assert!(matches!(result, Err(Error::ScheduleNotFound { .. })));
    }

    #[test]
    fn test_should_trigger_never_run() {
        let schedule = Schedule {
            id: "test".into(),
            name: "Every minute".into(),
            cron: "* * * * *".into(), // standard 5-field: every minute
            agent_id: "atlas".into(),
            title: "Test".into(),
            description: None,
            enabled: true,
            last_run: None,
            run_count: 0,
            created_at: String::new(),
            schedule_type: SCHEDULE_TYPE_CRON.into(),
            run_at: None,
            continuation_task_id: None,
            conversation_id: None,
            quota_gate: None,
            quota_deferred_until: None,
            quota_deferred_reason: None,
        };

        assert!(should_trigger(&schedule, Utc::now()));
    }

    #[test]
    fn test_should_trigger_recently_run() {
        // A schedule that just ran should not trigger again immediately
        let now = Utc::now();
        let schedule = Schedule {
            id: "test".into(),
            name: "Hourly".into(),
            cron: "0 * * * *".into(), // standard 5-field: top of every hour
            agent_id: "atlas".into(),
            title: "Test".into(),
            description: None,
            enabled: true,
            last_run: Some(now.format("%Y-%m-%d %H:%M:%S").to_string()),
            run_count: 1,
            created_at: String::new(),
            schedule_type: SCHEDULE_TYPE_CRON.into(),
            run_at: None,
            continuation_task_id: None,
            conversation_id: None,
            quota_gate: None,
            quota_deferred_until: None,
            quota_deferred_reason: None,
        };

        // Just ran — next match is next hour, so should not trigger now
        assert!(!should_trigger(&schedule, now));
    }

    #[test]
    fn test_should_trigger_invalid_cron() {
        let schedule = Schedule {
            id: "test".into(),
            name: "Bad".into(),
            cron: "not a cron".into(),
            agent_id: "atlas".into(),
            title: "Test".into(),
            description: None,
            enabled: true,
            last_run: None,
            run_count: 0,
            created_at: String::new(),
            schedule_type: SCHEDULE_TYPE_CRON.into(),
            run_at: None,
            continuation_task_id: None,
            conversation_id: None,
            quota_gate: None,
            quota_deferred_until: None,
            quota_deferred_reason: None,
        };

        assert!(!should_trigger(&schedule, Utc::now()));
    }

    #[test]
    fn test_trigger_enqueues_task() {
        let (db, mgr, board) = setup();
        let schedule = create_schedule(&mgr);

        let task = mgr
            .trigger(&schedule.id, &board)
            .unwrap()
            .into_task()
            .unwrap();

        // Verify task was enqueued
        let queue = TaskQueue::new(db);
        let pending = queue.pending_count("atlas").unwrap();
        assert_eq!(pending, 1);
        assert_eq!(task.agent_id.as_deref(), Some("atlas"));
    }

    #[test]
    fn continuation_wakeup_on_completed_task_bumps_ui_revisions_and_hides_fired_wakeup() {
        let (db, mgr, board) = setup();
        let task = board
            .create(&CreateTask {
                backlog: false,
                start_after: None,
                title: "Train monitor".to_string(),
                description: None,
                agent_id: Some("atlas".to_string()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        board
            .update_status(&task.id, "completed", Some("atlas"))
            .unwrap();

        let wakeup = mgr
            .create_one_shot(&CreateOneShotSchedule {
                name: "Check later".to_string(),
                run_at: None,
                delay_seconds: Some(60),
                agent_id: "atlas".to_string(),
                title: "Check status".to_string(),
                description: Some("Verify job status".to_string()),
                continuation_task_id: Some(task.id.clone()),
                conversation_id: None,
                quota_gate: None,
            })
            .unwrap();

        assert_eq!(mgr.list_filtered(None, false, false).unwrap().len(), 1);

        let before = db.ui_revisions();
        mgr.trigger(&wakeup.id, &board).unwrap();
        let after = db.ui_revisions();

        assert!(after.tasks > before.tasks);
        assert!(after.sessions > before.sessions);
        assert!(after.schedules > before.schedules);
        assert_eq!(board.get(&task.id).unwrap().status, TaskStatus::Pending);

        // Fired one-shot wakeups are excluded when include_completed_once is false,
        // while disabled cron schedules remain visible.
        let cron = create_schedule(&mgr);
        mgr.disable(&cron.id).unwrap();
        let filtered = mgr.list_filtered(None, false, false).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, cron.id);
        assert_eq!(mgr.list_filtered(None, false, true).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn quota_gated_schedule_defers_until_reset_then_creates_task() {
        use crate::usage::{window_from_used_pct, UsageAccountReport, UsageReportStatus};

        let (db, mgr, board) = setup();
        let due_at = DateTime::parse_from_rfc3339("2099-07-01T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let schedule = mgr
            .create_one_shot(&CreateOneShotSchedule {
                name: "Quota-aware nightly".into(),
                run_at: Some("2099-07-01T10:00:00Z".into()),
                delay_seconds: None,
                agent_id: "atlas".into(),
                title: "Run nightly checks".into(),
                description: None,
                continuation_task_id: None,
                conversation_id: None,
                quota_gate: Some(QuotaGatePolicy {
                    min_remaining_pct: 25.0,
                    window: "session".into(),
                    provider: Some("codex".into()),
                    reset_buffer_seconds: 60,
                    ..Default::default()
                }),
            })
            .unwrap();
        assert!(schedule.quota_gate.is_some());

        let usage = UsageService::new();
        usage
            .set_provider_override(
                "codex",
                vec![UsageAccountReport {
                    id: "codex:test".into(),
                    provider: "codex".into(),
                    provider_label: "Codex".into(),
                    harness: "Codex".into(),
                    account_label: None,
                    status: UsageReportStatus::Available,
                    plan_label: Some("Plus".into()),
                    observed_at: None,
                    fetched_at: "2099-07-01T10:00:00Z".into(),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "5-hour",
                        Some("5h".into()),
                        true,
                        Some(90.0),
                        Some(18_000),
                        Some("2099-07-01T12:00:00Z".into()),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;

        check_schedules_with_usage(&db, None, None, Some(&usage), due_at)
            .await
            .unwrap();
        let deferred = mgr.get(&schedule.id).unwrap();
        assert_eq!(deferred.run_count, 0);
        assert_eq!(
            deferred.quota_deferred_until.as_deref(),
            Some("2099-07-01T12:01:00Z")
        );

        // After reset, quota recovers and the scheduled task is created.
        usage
            .set_provider_override(
                "codex",
                vec![UsageAccountReport {
                    id: "codex:test".into(),
                    provider: "codex".into(),
                    provider_label: "Codex".into(),
                    harness: "Codex".into(),
                    account_label: None,
                    status: UsageReportStatus::Available,
                    plan_label: Some("Plus".into()),
                    observed_at: None,
                    fetched_at: "2099-07-01T12:01:00Z".into(),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "5-hour",
                        Some("5h".into()),
                        true,
                        Some(10.0),
                        Some(18_000),
                        Some("2099-07-01T17:00:00Z".into()),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;
        let after_reset = DateTime::parse_from_rfc3339("2099-07-01T12:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        check_schedules_with_usage(&db, None, None, Some(&usage), after_reset)
            .await
            .unwrap();
        let completed = mgr.get(&schedule.id).unwrap();
        assert_eq!(completed.run_count, 1);
        assert!(completed.quota_deferred_until.is_none());
        assert_eq!(board.list(None, None, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn deferred_recurring_schedule_that_never_ran_resumes_after_wait_expires() {
        use crate::usage::{window_from_used_pct, UsageAccountReport, UsageReportStatus};
        use chrono::Timelike;

        let (db, mgr, board) = setup();
        // Pick a concrete UTC timestamp, derive its local hour/minute, and build a daily cron for it.
        let due_at = DateTime::parse_from_rfc3339("2099-07-01T09:00:30Z")
            .unwrap()
            .with_timezone(&Utc);
        let due_local = due_at.with_timezone(&chrono::Local);
        let cron = format!("{} {} * * *", due_local.minute(), due_local.hour());

        let schedule = mgr
            .create(&CreateSchedule {
                name: "Daily quota-gated standup".into(),
                cron,
                agent_id: "atlas".into(),
                title: "Daily standup".into(),
                description: None,
                quota_gate: Some(QuotaGatePolicy {
                    min_remaining_pct: 25.0,
                    window: "session".into(),
                    provider: Some("codex".into()),
                    reset_buffer_seconds: 60,
                    ..Default::default()
                }),
            })
            .unwrap();
        assert!(schedule.last_run.is_none());
        assert_eq!(schedule.run_count, 0);

        let reset_at = due_at + chrono::Duration::hours(2);
        let expected_resume = reset_at + chrono::Duration::seconds(60);

        let usage = UsageService::new();
        usage
            .set_provider_override(
                "codex",
                vec![UsageAccountReport {
                    id: "codex:test".into(),
                    provider: "codex".into(),
                    provider_label: "Codex".into(),
                    harness: "Codex".into(),
                    account_label: None,
                    status: UsageReportStatus::Available,
                    plan_label: Some("Plus".into()),
                    observed_at: None,
                    fetched_at: due_at.to_rfc3339_opts(SecondsFormat::Secs, true),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "5-hour",
                        Some("5h".into()),
                        true,
                        Some(95.0),
                        Some(18_000),
                        Some(reset_at.to_rfc3339_opts(SecondsFormat::Secs, true)),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;

        // First occurrence matches cron and gets deferred until reset + 60s.
        check_schedules_with_usage(&db, None, None, Some(&usage), due_at)
            .await
            .unwrap();
        let deferred = mgr.get(&schedule.id).unwrap();
        assert_eq!(deferred.run_count, 0);
        assert!(deferred.last_run.is_none());
        assert_eq!(
            deferred.quota_deferred_until.as_deref(),
            Some(
                expected_resume
                    .to_rfc3339_opts(SecondsFormat::Secs, true)
                    .as_str()
            )
        );

        // Still waiting before expected_resume.
        let midpoint = due_at + chrono::Duration::hours(1);
        check_schedules_with_usage(&db, None, None, Some(&usage), midpoint)
            .await
            .unwrap();
        assert_eq!(mgr.get(&schedule.id).unwrap().run_count, 0);

        // Two hours later, `should_trigger` alone would be false because `last_run` is None
        // and the 2-minute initial lookback window has moved past the daily cron match.
        assert!(!should_trigger(&deferred, expected_resume));

        // After quota recovers, the expired deferral must still be rechecked and triggered.
        usage
            .set_provider_override(
                "codex",
                vec![UsageAccountReport {
                    id: "codex:test".into(),
                    provider: "codex".into(),
                    provider_label: "Codex".into(),
                    harness: "Codex".into(),
                    account_label: None,
                    status: UsageReportStatus::Available,
                    plan_label: Some("Plus".into()),
                    observed_at: None,
                    fetched_at: expected_resume.to_rfc3339_opts(SecondsFormat::Secs, true),
                    windows: vec![window_from_used_pct(
                        "five_hour",
                        "5-hour",
                        Some("5h".into()),
                        true,
                        Some(10.0),
                        Some(18_000),
                        Some(
                            (expected_resume + chrono::Duration::hours(5))
                                .to_rfc3339_opts(SecondsFormat::Secs, true),
                        ),
                        None,
                    )],
                    balances: Vec::new(),
                    details: Vec::new(),
                    problem: None,
                    error: None,
                    last_successful: None,
                }],
            )
            .await;

        check_schedules_with_usage(&db, None, None, Some(&usage), expected_resume)
            .await
            .unwrap();
        let resumed = mgr.get(&schedule.id).unwrap();
        assert_eq!(resumed.run_count, 1);
        assert!(resumed.quota_deferred_until.is_none());
        assert!(resumed.quota_deferred_reason.is_none());
        assert_eq!(board.list(None, None, 10).unwrap().len(), 1);
    }

    #[test]
    fn trigger_scheduled_skips_disabled_or_already_claimed_recurring_schedule() {
        let (_db, mgr, board) = setup();
        let snapshot = create_schedule(&mgr);

        // User disables the recurring schedule while an async quota check is in flight.
        mgr.disable(&snapshot.id).unwrap();
        assert!(mgr.trigger_scheduled(&snapshot, &board).unwrap().is_none());
        assert!(board.list(None, None, 10).unwrap().is_empty());
        assert_eq!(mgr.get(&snapshot.id).unwrap().run_count, 0);

        // Re-enabling allows claiming once, and a duplicate claim with the stale snapshot is ignored.
        let enabled = mgr.enable(&snapshot.id).unwrap();
        assert!(mgr.trigger_scheduled(&enabled, &board).unwrap().is_some());
        assert!(mgr.trigger_scheduled(&enabled, &board).unwrap().is_none());
        assert_eq!(board.list(None, None, 10).unwrap().len(), 1);
        assert_eq!(mgr.get(&snapshot.id).unwrap().run_count, 1);
    }
}
