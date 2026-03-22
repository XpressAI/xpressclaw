use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::tasks::board::{CreateTask, Task, TaskBoard};
use crate::tasks::queue::TaskQueue;

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

/// Manages cron-based schedules that create tasks when triggered.
///
/// Handles CRUD for schedule definitions and triggering (creating tasks).
/// The actual cron timer execution is handled by the server layer using
/// `tokio-cron-scheduler`.
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
        let task = board.create(&CreateTask {
            title,
            description,
            agent_id: Some(agent_id.clone()),
            parent_task_id: None,
            sop_id: None,
            conversation_id: None,
            priority: None,
            context: None,
        })?;

        // Enqueue for the dispatcher
        let queue = TaskQueue::new(self.db.clone());
        if let Err(e) = queue.enqueue(&task.id, &agent_id) {
            warn!(
                task_id = task.id.as_str(),
                schedule_id = id,
                error = %e,
                "failed to enqueue scheduled task"
            );
        }

        // Update last_run and run_count
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules SET last_run = ?1, run_count = run_count + 1 WHERE id = ?2",
                rusqlite::params![now_str, id],
            )
            .map_err(|e| Error::Database(e.to_string()))
        })?;

        Ok(task)
    }
}

// ---------------------------------------------------------------------------
// Background cron runner
// ---------------------------------------------------------------------------

/// Start the schedule runner background loop.
///
/// Checks all enabled schedules every 60 seconds and triggers any whose
/// cron expression matches the current time. Uses `croner` for cron parsing.
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
/// Checks if a cron match occurred between last_run and now.
/// If the schedule has never run, checks the last 2 minutes.
fn should_trigger(schedule: &Schedule, now: chrono::DateTime<Utc>) -> bool {
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
        .map(|dt| dt.and_utc())
        .unwrap_or_else(|| now - chrono::Duration::minutes(2));

    // Find the next cron match after last_run. If it's <= now, we should trigger.
    let mut iter = cron.iter_after(check_from);
    match iter.next() {
        Some(next) => next <= now,
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
