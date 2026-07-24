use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::sessions::{NewEvent, SessionManager};
use crate::tasks::board::{CreateTask, Task, TaskBoard, TaskStatus};
use crate::tasks::conversation::TaskConversation;
use crate::tasks::queue::TaskQueue;

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
}

/// Request to create a new schedule.
#[derive(Debug, Deserialize)]
pub struct CreateSchedule {
    pub name: String,
    pub cron: String,
    pub agent_id: String,
    pub title: String,
    pub description: Option<String>,
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
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO schedules (id, name, cron, agent_id, title, description, enabled, run_count, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, 0, ?7)",
                rusqlite::params![id, req.name, req.cron, req.agent_id, req.title, req.description, now],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        self.get(&id)
    }

    /// Create a one-shot schedule that is disabled after its first run.
    pub fn create_one_shot(&self, req: &CreateOneShotSchedule) -> Result<Schedule> {
        validate_required("name", &req.name)?;
        validate_required("agent_id", &req.agent_id)?;
        validate_required("title", &req.title)?;
        if let Some(task_id) = req.continuation_task_id.as_deref() {
            let task = TaskBoard::new(self.db.clone()).get(task_id).map_err(|_| {
                Error::Schedule(format!("continuation task '{task_id}' was not found"))
            })?;
            if task.agent_id.as_deref() != Some(req.agent_id.as_str()) {
                return Err(Error::Schedule(format!(
                    "continuation task '{task_id}' does not belong to project '{}'",
                    req.agent_id
                )));
            }
        }

        let run_at = resolve_one_shot_deadline(req)?.to_rfc3339_opts(SecondsFormat::Secs, true);
        let id = Uuid::new_v4().to_string();
        let now = Utc::now()
            .naive_utc()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO schedules
                 (id, name, cron, agent_id, title, description, enabled, run_count, created_at,
                  schedule_type, run_at, continuation_task_id)
                 VALUES (?1, ?2, '', ?3, ?4, ?5, 1, 0, ?6, ?7, ?8, ?9)",
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
                ],
            )
            .map_err(|e| Error::Database(e.to_string()))
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
        self.db.with_conn(|conn| {
            let mut sql = "SELECT * FROM schedules WHERE 1=1".to_string();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(aid) = agent_id {
                sql.push_str(" AND agent_id = ?");
                params.push(Box::new(aid.to_string()));
            }
            if enabled_only {
                sql.push_str(" AND enabled = 1");
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
        if enabled {
            let schedule = self.get(id)?;
            if schedule.schedule_type == SCHEDULE_TYPE_ONCE && schedule.run_count > 0 {
                return Err(Error::Schedule(
                    "a completed one-shot schedule cannot be enabled again".to_string(),
                ));
            }
        }

        let affected = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules SET enabled = ?1 WHERE id = ?2",
                rusqlite::params![enabled as i32, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        if affected == 0 {
            return Err(Error::ScheduleNotFound { id: id.to_string() });
        }
        self.get(id)
    }

    /// Trigger a schedule immediately, creating a task and enqueuing it.
    ///
    /// Supports placeholders in the title:
    /// - `{date}` → current date (YYYY-MM-DD)
    /// - `{time}` → current time (HH:MM)
    /// - `{datetime}` → current datetime
    pub fn trigger(&self, id: &str, board: &TaskBoard) -> Result<Task> {
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
        let task = if let Some(task_id) = schedule.continuation_task_id.as_deref() {
            match self.enqueue_continuation(
                &schedule,
                task_id,
                &title,
                description.as_deref(),
                board,
            ) {
                Ok(task) => task,
                Err(error) => {
                    if is_one_shot {
                        self.release_one_shot_claim(id);
                    }
                    return Err(error);
                }
            }
        } else {
            let task = match board.create(&CreateTask {
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
            }) {
                Ok(task) => task,
                Err(error) => {
                    if is_one_shot {
                        self.release_one_shot_claim(id);
                    }
                    return Err(error);
                }
            };

            // Standalone and recurring schedules retain their existing
            // create-a-task behavior.
            let queue = TaskQueue::new(self.db.clone());
            if let Err(error) = queue.enqueue(&task.id, &agent_id) {
                if is_one_shot {
                    self.release_one_shot_claim(id);
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
            task
        };

        // Update last_run and run_count
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules
                 SET last_run = ?1,
                     run_count = run_count + 1,
                     enabled = CASE WHEN schedule_type = 'once' THEN 0 ELSE enabled END
                 WHERE id = ?2",
                rusqlite::params![now_str, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(task)
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
        if task.agent_id.as_deref() != Some(schedule.agent_id.as_str()) {
            return Err(Error::Schedule(format!(
                "continuation task '{task_id}' does not belong to project '{}'",
                schedule.agent_id
            )));
        }

        let instruction = description
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(title);
        let content = format!("Scheduled wake-up: {}\n\n{instruction}", schedule.name);
        let conversation = TaskConversation::new(self.db.clone());
        let message = conversation.add_message(task_id, "user", &content)?;
        let queue = TaskQueue::new(self.db.clone());
        if let Err(error) = queue.enqueue_continuation(task_id, &schedule.agent_id) {
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

    fn release_one_shot_claim(&self, id: &str) {
        let _ = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules SET enabled = 1 WHERE id = ?1 AND run_count = 0",
                [id],
            )
        });
    }
}

// ---------------------------------------------------------------------------
// Background cron runner
// ---------------------------------------------------------------------------

/// Start the schedule runner background loop.
///
/// Checks all enabled schedules every 60 seconds and triggers due one-shot
/// deadlines or cron expressions that match the current time.
pub async fn start_schedule_runner(db: Arc<Database>) {
    info!("schedule runner started");

    loop {
        if let Err(e) = check_schedules(&db) {
            error!(error = %e, "schedule check error");
        }
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }
}

fn check_schedules(db: &Arc<Database>) -> Result<()> {
    let mgr = ScheduleManager::new(db.clone());
    let board = TaskBoard::new(db.clone());

    let schedules = mgr.list(None, true)?; // enabled only
    if schedules.is_empty() {
        return Ok(());
    }

    let now = Utc::now();

    for schedule in &schedules {
        if should_trigger(schedule, now) {
            info!(
                schedule_id = schedule.id.as_str(),
                name = schedule.name.as_str(),
                agent_id = schedule.agent_id.as_str(),
                "triggering scheduled task"
            );
            match mgr.trigger(&schedule.id, &board) {
                Ok(task) => {
                    info!(
                        schedule_id = schedule.id.as_str(),
                        task_id = task.id.as_str(),
                        "scheduled task created"
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

fn row_to_schedule(row: &rusqlite::Row) -> Schedule {
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
    }
}

fn validate_required(name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::Schedule(format!("{name} cannot be empty")));
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
    fn test_trigger_creates_task() {
        let (_, mgr, board) = setup();
        let schedule = create_schedule(&mgr);

        let task = mgr.trigger(&schedule.id, &board).unwrap();

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

        let task = mgr.trigger(&schedule.id, &board).unwrap();
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
            })
            .unwrap();

        let triggered = mgr.trigger(&schedule.id, &board).unwrap();

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
        });

        assert!(matches!(result, Err(Error::Schedule(_))));
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
            })
            .unwrap();

        check_schedules(&db).unwrap();
        check_schedules(&db).unwrap();

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
        };

        assert!(!should_trigger(&schedule, Utc::now()));
    }

    #[test]
    fn test_trigger_enqueues_task() {
        let (db, mgr, board) = setup();
        let schedule = create_schedule(&mgr);

        let task = mgr.trigger(&schedule.id, &board).unwrap();

        // Verify task was enqueued
        let queue = TaskQueue::new(db);
        let pending = queue.pending_count("atlas").unwrap();
        assert_eq!(pending, 1);
        assert_eq!(task.agent_id.as_deref(), Some("atlas"));
    }
}
