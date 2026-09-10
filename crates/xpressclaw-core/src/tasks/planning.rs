//! Planning is a durable constraint on ordinary tasks, never a second scheduler
//! that manufactures tasks or transitions an agent into working/completed.
use super::{
    board::{
        append_task_search, ensure_task_agent_project, refresh_logical_session_status, row_to_task,
        Task,
    },
    queue::TaskQueue,
};
use crate::{
    db::Database,
    error::{Error, Result},
    projects::ensure_project_accepts_work,
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

pub fn normalize_start_after(value: Option<&str>) -> Result<Option<String>> {
    value
        .map(|value| {
            let time = DateTime::parse_from_rfc3339(value)
                .map_err(|_| {
                    Error::Task(
                        "Start no earlier than must be an RFC 3339 timestamp with a timezone"
                            .into(),
                    )
                })?
                .with_timezone(&Utc);
            // SQLite date comparisons have millisecond precision. Round up,
            // never down, so sub-millisecond input cannot start work early.
            let remainder = time.timestamp_subsec_nanos() % 1_000_000;
            let time = if remainder == 0 {
                time
            } else {
                time + chrono::Duration::nanoseconds((1_000_000 - remainder).into())
            };
            // Check the UTC year that SQLite will receive, including rounding carry.
            if !(1970..=9999).contains(&chrono::Datelike::year(&time)) {
                return Err(Error::Task(
                    "Schedule year must be between 1970 and 9999".into(),
                ));
            }
            Ok(time.to_rfc3339_opts(SecondsFormat::Millis, true))
        })
        .transpose()
}

pub fn is_scheduled(value: Option<&str>) -> bool {
    value
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|time| time > Utc::now())
}

// Both claim paths use this predicate. Enqueue/continuation/recovery can retain
// one durable queued attempt; none may acquire an execution lease early.
pub(super) const ELIGIBLE: &str = "
    t.status NOT IN ('completed', 'cancelled')
    AND t.backlog = 0
    AND (t.start_after IS NULL OR julianday(t.start_after) <= julianday(?1))
    AND NOT EXISTS (SELECT 1 FROM projects p WHERE p.id = t.project_id AND p.deletion_started_at IS NOT NULL)
    AND NOT EXISTS (
      SELECT 1 FROM task_dependencies d JOIN tasks dependency ON dependency.id = d.depends_on_id
      WHERE d.task_id = t.id AND dependency.status != 'completed'
    )
    AND NOT EXISTS (
      SELECT 1 FROM tasks child
      WHERE child.parent_task_id = t.id AND child.blocks_parent = 1 AND child.status != 'completed'
    )";

const SNAPSHOT: &str = "WITH facts AS (
 SELECT tasks.*,
   EXISTS(SELECT 1 FROM task_queue q WHERE q.task_id = tasks.id AND q.status = 'running') AS claimed,
   EXISTS(SELECT 1 FROM work_attempts a WHERE a.task_id = tasks.id AND a.status IN ('preparing','running')) AS live,
   EXISTS(SELECT 1 FROM task_queue q WHERE q.task_id = tasks.id AND q.status = 'queued') AS queued,
   EXISTS(SELECT 1 FROM task_pull_requests pr WHERE pr.task_id = tasks.id AND pr.status IN ('waiting','attention')) OR EXISTS(SELECT 1 FROM work_attempts a WHERE a.task_id = tasks.id AND a.status = 'review') AS reviewing,
   EXISTS(SELECT 1 FROM task_dependencies d JOIN tasks t ON t.id = d.depends_on_id WHERE d.task_id = tasks.id AND t.status != 'completed') AS dependencies_blocked,
   EXISTS(SELECT 1 FROM tasks child WHERE child.parent_task_id = tasks.id AND child.blocks_parent = 1 AND child.status != 'completed') AS children_blocked,
   (SELECT MIN(a.started_at) FROM work_attempts a WHERE a.task_id = tasks.id) AS actual_started_at,
   (SELECT json_group_array(d.depends_on_id) FROM task_dependencies d WHERE d.task_id = tasks.id) AS dependency_ids
 FROM tasks WHERE hidden = 0 AND provenance != 'native_plan'
), planned AS (
 SELECT facts.*,
 CASE
   WHEN status IN ('completed','cancelled') THEN 'done'
   WHEN status = 'waiting_for_input' OR EXISTS(SELECT 1 FROM work_attempts a WHERE a.task_id = facts.id AND a.status = 'waiting_for_input') THEN 'input'
   WHEN live THEN 'working'
   WHEN reviewing THEN 'review'
   WHEN claimed THEN 'working'
   WHEN backlog THEN 'backlog'
   WHEN dependencies_blocked OR children_blocked OR status = 'blocked' THEN 'blocked'
   WHEN start_after IS NOT NULL AND julianday(start_after) > julianday('now') THEN 'scheduled'
   ELSE 'queue'
 END AS lane,
 CASE
   WHEN status IN ('completed','cancelled') THEN 'Finished tasks cannot be planned. Open task details to reopen work.'
   WHEN claimed OR live THEN 'This task has an active turn. Planning cannot interrupt it.'
   WHEN reviewing THEN 'Pull-request review must be handled in task details.'
   WHEN status IN ('waiting_for_input','blocked') OR EXISTS(SELECT 1 FROM work_attempts a WHERE a.task_id = facts.id AND a.status = 'waiting_for_input') THEN 'Respond or retry in task details before planning another turn.'
   ELSE NULL
 END AS planning_disabled_reason
 FROM facts
) ";

#[derive(Debug, Serialize)]
pub struct PlanningTask {
    #[serde(flatten)]
    pub task: Task,
    pub planning: PlanningState,
    pub depends_on: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct PlanningState {
    pub lane: String,
    pub disabled_reason: Option<String>,
    pub queued: bool,
    pub actual_started_at: Option<String>,
}
#[derive(Default, Deserialize)]
pub struct PlanningFilter {
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
#[derive(Serialize)]
pub struct PlanningPage {
    pub tasks: Vec<PlanningTask>,
    pub total: i64,
    pub counts: BTreeMap<String, i64>,
}
#[derive(Deserialize)]
pub struct PlanningChange {
    pub expected_revision: i64,
    #[serde(flatten)]
    pub change: PlanningAction,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PlanningAction {
    Schedule {
        start_after: Option<String>,
        #[serde(default)]
        activate: bool,
    },
    Backlog,
    Queue,
    Assign {
        agent_id: String,
    },
    Priority {
        priority: i32,
    },
    Reorder {
        before_id: Option<String>,
        after_id: Option<String>,
    },
}

pub struct TaskPlanner {
    db: Arc<Database>,
}
impl TaskPlanner {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn list(&self, filter: &PlanningFilter) -> Result<PlanningPage> {
        if filter
            .search
            .as_ref()
            .is_some_and(|s| s.chars().count() > 200)
        {
            return Err(Error::Task(
                "Task search must be 200 characters or fewer".into(),
            ));
        }
        self.db.with_conn(|conn| {
            let mut predicate = " FROM planned tasks WHERE 1=1".to_string();
            let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
            for (column, value) in [("project_id", &filter.project_id), ("agent_id", &filter.agent_id)] {
                if let Some(value) = value.as_ref().filter(|s| !s.is_empty()) {
                    predicate.push_str(&format!(" AND {column} = ?")); values.push(Box::new(value.clone()));
                }
            }
            append_task_search(&mut predicate, &mut values, filter.search.as_deref());
            let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
            let mut counts = BTreeMap::new();
            for row in conn.prepare(&format!("{SNAPSHOT} SELECT lane, COUNT(*) {predicate} GROUP BY lane"))?
                .query_map(refs.as_slice(), |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))? {
                let (lane, count) = row?; counts.insert(lane, count);
            }
            let selected = filter.status.as_deref().unwrap_or("active");
            match selected {
                "all" => {},
                "active" => predicate.push_str(" AND lane != 'done'"),
                "attention" => predicate.push_str(" AND lane IN ('input','blocked','review')"),
                lane => { predicate.push_str(" AND lane = ?"); values.push(Box::new(lane.to_string())); }
            }
            let total = counts.iter().filter(|(lane, _)| match selected {
                "all" => true, "active" => lane.as_str() != "done", "attention" => ["input","blocked","review"].contains(&lane.as_str()), selected => lane.as_str() == selected,
            }).map(|(_, count)| count).sum();
            values.push(Box::new(filter.limit.unwrap_or(100).clamp(1, 200)));
            values.push(Box::new(filter.offset.unwrap_or(0).max(0)));
            let refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
            let sql = format!("{SNAPSHOT} SELECT tasks.* {predicate} ORDER BY priority DESC, position, id LIMIT ? OFFSET ?");
            let mut statement = conn.prepare(&sql)?;
            let mut rows = statement.query(refs.as_slice())?;
            let mut tasks = vec![];
            while let Some(row) = rows.next()? { tasks.push(planning_row(row)?); }
            Ok(PlanningPage { tasks, total, counts })
        })
    }

    pub fn get(&self, id: &str) -> Result<PlanningTask> {
        self.db.with_conn(|conn| read_planning(conn, id))
    }

    pub fn change(&self, id: &str, request: &PlanningChange) -> Result<PlanningTask> {
        self.db.with_conn(|conn| {
            let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
            let current = read_planning(&tx, id)?;
            if current.task.revision != request.expected_revision {
                return Err(Error::Task("Task changed since it was loaded. Refresh and try again.".into()));
            }
            if let Some(reason) = current.planning.disabled_reason { return Err(Error::Task(reason)); }
            if let Some(project) = current.task.project_id.as_deref() { ensure_project_accepts_work(&tx, project)?; }
            match &request.change {
                PlanningAction::Schedule { start_after, activate } => {
                    let value = normalize_start_after(start_after.as_deref())?;
                    tx.execute("UPDATE tasks SET start_after = ?1, backlog = CASE WHEN ?2 THEN 0 ELSE backlog END WHERE id = ?3", params![value, activate, id])?;
                }
                PlanningAction::Backlog => { tx.execute("UPDATE tasks SET backlog = 1 WHERE id = ?1", [id])?; }
                PlanningAction::Queue => {
                    // Queuing does not waive dependencies or blocking child work.
                    tx.execute("UPDATE tasks SET backlog = 0 WHERE id = ?1", [id])?;
                }
                PlanningAction::Priority { priority } => {
                    if !(-1000..=1000).contains(priority) { return Err(Error::Task("Priority must be between -1000 and 1000".into())); }
                    tx.execute("UPDATE tasks SET priority = ?1 WHERE id = ?2", params![priority, id])?;
                }
                PlanningAction::Assign { agent_id } => {
                    if agent_id.is_empty() { return Err(Error::Task("Choose an assigned agent".into())); }
                    ensure_task_agent_project(&tx, id, agent_id)?;
                    tx.execute("INSERT OR IGNORE INTO logical_sessions (id, agent_id) VALUES (?1, ?1)", [agent_id])?;
                    tx.execute("UPDATE session_events SET session_id = ?1 WHERE attempt_id IN (SELECT attempt_id FROM task_queue WHERE task_id = ?2 AND status = 'queued')", params![agent_id, id])?;
                    tx.execute("UPDATE attempt_artifacts SET session_id = ?1 WHERE attempt_id IN (SELECT attempt_id FROM task_queue WHERE task_id = ?2 AND status = 'queued')", params![agent_id, id])?;
                    tx.execute("UPDATE work_attempts SET session_id = ?1 WHERE task_id = ?2 AND status = 'queued'", params![agent_id, id])?;
                    tx.execute("UPDATE task_queue SET agent_id = ?1 WHERE task_id = ?2 AND status = 'queued'", params![agent_id, id])?;
                    tx.execute("UPDATE tasks SET agent_id = ?1, session_id = ?1 WHERE id = ?2", params![agent_id, id])?;
                    if let Some(previous) = current.task.agent_id.as_deref() { refresh_logical_session_status(&tx, previous)?; }
                    refresh_logical_session_status(&tx, agent_id)?;
                }
                PlanningAction::Reorder { before_id, after_id } => {
                    let (target, before) = match (before_id, after_id) {
                        (Some(target), None) => (target, true), (None, Some(target)) => (target, false),
                        _ => return Err(Error::Task("Choose exactly one neighboring task".into())),
                    };
                    if target == id { return Err(Error::Task("Choose a different neighboring task".into())); }
                    let target = read_planning(&tx, target)?;
                    if target.planning.disabled_reason.is_some() || target.planning.lane != current.planning.lane {
                        return Err(Error::Task("Reorder within the same editable column".into()));
                    }
                    let mut position = insertion_position(&tx, id, &target.task, before)?;
                    if position.is_none() {
                        // Rebalance only when the gap runs out of precision. Normal
                        // moves must not invalidate every other task's revision.
                        // Snapshot ranks before writing: an inlined CTE can
                        // otherwise recalculate them from partially updated rows.
                        tx.execute("WITH ranks AS MATERIALIZED (SELECT id, ROW_NUMBER() OVER (ORDER BY position, id) * 1024.0 AS rank FROM tasks WHERE priority = ?1) UPDATE tasks SET position = (SELECT rank FROM ranks WHERE ranks.id = tasks.id) WHERE priority = ?1", [target.task.priority])?;
                        position = insertion_position(&tx, id, &read_planning(&tx, &target.task.id)?.task, before)?;
                    }
                    let position = position.ok_or_else(|| Error::Task("Task order changed. Refresh and try again.".into()))?;
                    tx.execute("UPDATE tasks SET priority = ?1, position = ?2 WHERE id = ?3", params![target.task.priority, position, id])?;
                }
            }
            tx.execute("UPDATE tasks SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1", [id])?;
            // A schedule on an idle task creates one continuation, never another
            // task identity. Existing queued work is coalesced by enqueue.
            if matches!(request.change, PlanningAction::Queue | PlanningAction::Schedule { .. } | PlanningAction::Assign { .. }) {
                let updated = read_planning(&tx, id)?;
                if let Some(agent_id) = updated.task.agent_id.as_deref().filter(|_| !updated.task.backlog) {
                    TaskQueue::enqueue_in_transaction(&tx, id, agent_id)?;
                }
            }
            let updated = read_planning(&tx, id)?;
            tx.commit()?;
            Ok(updated)
        })
    }
}
fn insertion_position(
    conn: &rusqlite::Connection,
    moving_id: &str,
    target: &Task,
    before: bool,
) -> Result<Option<f64>> {
    let comparison = if before { "<=" } else { ">=" };
    let aggregate = if before { "MAX" } else { "MIN" };
    let neighbor: Option<f64> = conn.query_row(
        &format!("SELECT {aggregate}(position) FROM tasks WHERE priority = ?1 AND position {comparison} ?2 AND id NOT IN (?3, ?4)"),
        params![target.priority, target.position, moving_id, target.id], |row| row.get(0),
    )?;
    let edge = neighbor.unwrap_or(target.position + if before { -2048.0 } else { 2048.0 });
    let midpoint = target.position / 2.0 + edge / 2.0;
    Ok(
        (midpoint.is_finite() && midpoint != target.position && midpoint != edge)
            .then_some(midpoint),
    )
}
fn read_planning(conn: &rusqlite::Connection, id: &str) -> Result<PlanningTask> {
    let sql = format!("{SNAPSHOT} SELECT * FROM planned WHERE id = ?1");
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query([id])?;
    let row = rows
        .next()?
        .ok_or_else(|| Error::TaskNotFound { id: id.into() })?;
    planning_row(row)
}
fn planning_row(row: &rusqlite::Row) -> Result<PlanningTask> {
    Ok(PlanningTask {
        task: row_to_task(row)?,
        planning: PlanningState {
            lane: row.get("lane")?,
            disabled_reason: row.get("planning_disabled_reason")?,
            queued: row.get("queued")?,
            actual_started_at: row.get("actual_started_at")?,
        },
        depends_on: serde_json::from_str(&row.get::<_, String>("dependency_ids")?)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::board::{CreateTask, ReportedSubtask, TaskBoard, TaskStatus};
    use super::*;
    use crate::agents::registry::AgentRegistry;
    fn setup(db: Arc<Database>) -> (TaskBoard, TaskQueue, TaskPlanner) {
        AgentRegistry::new(db.clone())
            .ensure("atlas", "native")
            .unwrap();
        AgentRegistry::new(db.clone())
            .ensure("helper", "native")
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE agents SET project_id = 'atlas' WHERE id = 'helper'",
                [],
            )
        })
        .unwrap();
        (
            TaskBoard::new(db.clone()),
            TaskQueue::new(db.clone()),
            TaskPlanner::new(db),
        )
    }
    fn time(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }
    fn create(board: &TaskBoard, title: &str, date: Option<&str>, priority: i32) -> Task {
        board
            .create(&CreateTask {
                title: title.into(),
                agent_id: Some("atlas".into()),
                start_after: date.map(String::from),
                priority: Some(priority),
                ..Default::default()
            })
            .unwrap()
    }
    fn change(planner: &TaskPlanner, id: &str, change: PlanningAction) -> PlanningTask {
        let revision = planner.get(id).unwrap().task.revision;
        planner
            .change(
                id,
                &PlanningChange {
                    expected_revision: revision,
                    change,
                },
            )
            .unwrap()
    }
    #[test]
    fn queued_parent_continuations_wait_for_blocking_children_in_both_claim_paths() {
        for by_agent in [false, true] {
            let db = Arc::new(Database::open_memory().unwrap());
            let (board, queue, planner) = setup(db.clone());
            let sessions = crate::sessions::SessionManager::new(db);
            let claim = || {
                (if by_agent {
                    queue.claim("atlas")
                } else {
                    queue.claim_next()
                })
                .unwrap()
            };
            let finish = |item: &super::super::queue::QueueItem| {
                sessions
                    .transition_attempt(
                        item.attempt_id.as_deref().unwrap(),
                        "completed",
                        "Turn finished",
                        None,
                        None,
                    )
                    .unwrap();
                queue.complete(item.id, "Turn finished").unwrap();
            };
            let parent = create(&board, "Parent", None, 100);
            let initial = queue.enqueue(&parent.id, "atlas").unwrap();
            assert_eq!(claim().unwrap().id, initial.id);
            let child = board
                .create(&CreateTask {
                    title: "Required child".into(),
                    agent_id: Some("helper".into()),
                    parent_task_id: Some(parent.id.clone()),
                    ..Default::default()
                })
                .unwrap();
            let child_turn = queue.enqueue(&child.id, "helper").unwrap();
            assert_eq!(queue.claim("helper").unwrap().unwrap().id, child_turn.id);
            sessions
                .transition_attempt(
                    child_turn.attempt_id.as_deref().unwrap(),
                    "running",
                    "Child running",
                    None,
                    None,
                )
                .unwrap();
            board.update_status(&child.id, "in_progress", None).unwrap();
            let continuation = queue
                .enqueue_continuation(&parent.id, "atlas")
                .unwrap()
                .unwrap();
            finish(&initial);
            assert_eq!(planner.get(&parent.id).unwrap().planning.lane, "blocked");

            // The high-priority parent must not run or starve other ready work
            // after releasing its session while the required child runs.
            let ready = create(&board, "Independent work", None, 0);
            let ready_turn = queue.enqueue(&ready.id, "atlas").unwrap();
            assert_eq!(
                claim().unwrap().id,
                ready_turn.id,
                "claim by agent: {by_agent}"
            );
            finish(&ready_turn);
            assert!(claim().is_none());
            assert_eq!(queue.get(continuation.id).unwrap().status, "queued");

            finish(&child_turn);
            board.update_status(&child.id, "completed", None).unwrap();
            assert_eq!(planner.get(&parent.id).unwrap().planning.lane, "queue");
            assert_eq!(claim().unwrap().id, continuation.id);
            assert!(claim().is_none());
        }
    }

    #[test]
    fn blocking_child_gates_survive_restart_and_ignore_nonblocking_plan_rows() {
        let folder = tempfile::tempdir().unwrap();
        let filename = folder.path().join("children.db");
        let (parent_id, child_id, continuation_id) = {
            let (board, queue, _) = setup(Arc::new(Database::open(&filename).unwrap()));
            let parent = create(&board, "Parent", None, 0);
            let child = board
                .create(&CreateTask {
                    title: "Required child".into(),
                    parent_task_id: Some(parent.id.clone()),
                    agent_id: Some("helper".into()),
                    ..Default::default()
                })
                .unwrap();
            let continuation = queue
                .enqueue_continuation(&parent.id, "atlas")
                .unwrap()
                .unwrap();
            board
                .sync_reported_subtasks(
                    &parent.id,
                    continuation.attempt_id.as_deref().unwrap(),
                    &[ReportedSubtask {
                        title: "Current-turn checklist".into(),
                        status: TaskStatus::Pending,
                    }],
                )
                .unwrap();
            (parent.id, child.id, continuation.id)
        };
        let (board, queue, planner) = setup(Arc::new(Database::open(&filename).unwrap()));
        queue.recover_in_progress().unwrap();
        for status in [
            "pending",
            "in_progress",
            "waiting_for_input",
            "blocked",
            "cancelled",
        ] {
            board.update_status(&child_id, status, None).unwrap();
            assert_eq!(planner.get(&parent_id).unwrap().planning.lane, "blocked");
            assert!(
                queue.claim("atlas").unwrap().is_none(),
                "child status: {status}"
            );
            assert!(
                queue.claim_next().unwrap().is_none(),
                "child status: {status}"
            );
        }
        assert_eq!(queue.get(continuation_id).unwrap().status, "queued");
        board.update_status(&child_id, "completed", None).unwrap();
        assert!(board
            .list_subtasks(&parent_id)
            .unwrap()
            .iter()
            .any(|task| !task.blocks_parent && task.status == TaskStatus::Pending));
        assert_eq!(planner.get(&parent_id).unwrap().planning.lane, "queue");
        assert_eq!(queue.claim_next().unwrap().unwrap().id, continuation_id);
        assert!(queue.claim_next().unwrap().is_none());
    }

    #[test]
    fn threshold_is_inclusive_and_future_priority_cannot_starve_ready_work() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (board, queue, _) = setup(db.clone());
        let delayed = create(&board, "Later", Some("2100-01-01T09:00:00+09:00"), 10);
        assert_eq!(
            delayed.start_after.as_deref(),
            Some("2100-01-01T00:00:00.000Z")
        );
        let first = queue.enqueue(&delayed.id, "atlas").unwrap();
        assert_eq!(first.id, queue.enqueue(&delayed.id, "atlas").unwrap().id);
        assert!(queue
            .enqueue_continuation(&delayed.id, "atlas")
            .unwrap()
            .is_none());
        assert!(queue
            .ensure_enqueued(&delayed.id, "atlas")
            .unwrap()
            .is_none());
        let ready = create(&board, "Ready", None, 0);
        let ready_item = queue.enqueue(&ready.id, "atlas").unwrap();
        let before = time("2099-12-31T23:59:59.999Z");
        assert_eq!(
            queue.claim_at(None, before).unwrap().unwrap().id,
            ready_item.id
        );
        queue.complete(ready_item.id, "done").unwrap();
        crate::sessions::SessionManager::new(db.clone())
            .transition_attempt(
                ready_item.attempt_id.as_deref().unwrap(),
                "completed",
                "done",
                None,
                None,
            )
            .unwrap();
        assert!(queue.claim_at(Some("atlas"), before).unwrap().is_none());
        assert_eq!(
            queue
                .claim_at(None, time("2100-01-01T00:00:00Z"))
                .unwrap()
                .unwrap()
                .id,
            first.id
        );
        assert!(queue
            .claim_at(None, time("2100-01-01T00:00:01Z"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn schedules_reject_out_of_range_utc_years_without_mutating_tasks() {
        let (board, _, planner) = setup(Arc::new(Database::open_memory().unwrap()));
        let original = board
            .create(&CreateTask {
                title: "Keep the existing plan".into(),
                agent_id: Some("atlas".into()),
                start_after: Some("2100-01-01T00:00:00Z".into()),
                backlog: true,
                ..Default::default()
            })
            .unwrap();
        for value in [
            "9999-12-31T23:59:59-01:00",
            "9999-12-31T22:59:59.999000001-01:00",
            "9999-12-31T23:59:59.999000001Z",
            "1970-01-01T00:00:00+01:00",
            "1969-12-31T23:59:59.998999999Z",
        ] {
            assert!(
                board
                    .create(&CreateTask {
                        title: "Invalid schedule".into(),
                        agent_id: Some("atlas".into()),
                        start_after: Some(value.into()),
                        ..Default::default()
                    })
                    .is_err(),
                "creation accepted {value}"
            );
            assert!(
                planner
                    .change(
                        &original.id,
                        &PlanningChange {
                            expected_revision: original.revision,
                            change: PlanningAction::Schedule {
                                start_after: Some(value.into()),
                                activate: true,
                            },
                        },
                    )
                    .is_err(),
                "planning edit accepted {value}"
            );
            let unchanged = planner.get(&original.id).unwrap();
            assert_eq!(unchanged.task.start_after, original.start_after);
            assert_eq!(unchanged.task.revision, original.revision);
            assert!(unchanged.task.backlog);
            assert!(!unchanged.planning.queued);
        }
        assert_eq!(planner.list(&PlanningFilter::default()).unwrap().total, 1);
    }

    #[test]
    fn schedules_at_normalized_utc_year_boundaries_remain_claimable() {
        for (input, expected) in [
            ("1969-12-31T23:00:00-01:00", "1970-01-01T00:00:00.000Z"),
            ("1969-12-31T23:59:59.999999999Z", "1970-01-01T00:00:00.000Z"),
            ("1970-01-01T00:00:00.000000001Z", "1970-01-01T00:00:00.001Z"),
            (
                "9999-12-31T22:59:59.998000001-01:00",
                "9999-12-31T23:59:59.999Z",
            ),
            ("9999-12-31T23:59:59.999Z", "9999-12-31T23:59:59.999Z"),
        ] {
            for agent in [None, Some("atlas")] {
                let (board, queue, planner) = setup(Arc::new(Database::open_memory().unwrap()));
                let task = create(&board, "Boundary schedule", Some(input), 0);
                assert_eq!(task.start_after.as_deref(), Some(expected));
                assert_eq!(
                    planner.get(&task.id).unwrap().planning.lane,
                    if is_scheduled(Some(expected)) {
                        "scheduled"
                    } else {
                        "queue"
                    }
                );
                let queued = queue.enqueue(&task.id, "atlas").unwrap();
                let threshold = time(expected);
                assert!(queue
                    .claim_at(agent, threshold - chrono::Duration::milliseconds(1))
                    .unwrap()
                    .is_none());
                assert_eq!(
                    queue.claim_at(agent, threshold).unwrap().unwrap().id,
                    queued.id,
                    "input: {input}"
                );
                assert!(queue.claim_at(agent, threshold).unwrap().is_none());
            }
        }
    }

    #[test]
    fn schedule_survives_reopening_and_recovery_on_the_same_task() {
        let folder = tempfile::tempdir().unwrap();
        let filename = folder.path().join("planning.db");
        let id;
        {
            let (board, queue, _) = setup(Arc::new(Database::open(&filename).unwrap()));
            let task = create(&board, "Persistent", Some("2100-03-01T00:00:00Z"), 0);
            id = task.id;
            queue.enqueue(&id, "atlas").unwrap();
        }
        let (board, queue, _) = setup(Arc::new(Database::open(&filename).unwrap()));
        queue.recover_in_progress().unwrap();
        assert_eq!(
            board.get(&id).unwrap().start_after.as_deref(),
            Some("2100-03-01T00:00:00.000Z")
        );
        assert!(queue.claim_next().unwrap().is_none());
        assert!(queue.enqueue_continuation(&id, "atlas").unwrap().is_none());
        assert_eq!(
            queue
                .claim_at(None, time("2100-03-02T00:00:00Z"))
                .unwrap()
                .unwrap()
                .task_id,
            id
        );
    }
    #[test]
    fn backlog_stays_parked_through_assignment_schedule_recovery_and_continuations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("backlog.db");
        let db = Arc::new(Database::open(&path).unwrap());
        let (board, queue, planner) = setup(db);
        let request: CreateTask = serde_json::from_value(serde_json::json!({
            "title": "Default to do", "agent_id": "atlas"
        }))
        .unwrap();
        assert!(!board.create(&request).unwrap().backlog);
        let task = board
            .create(&CreateTask {
                title: "Keep an idea for later".into(),
                agent_id: Some("atlas".into()),
                backlog: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(planner.get(&task.id).unwrap().planning.lane, "backlog");
        change(
            &planner,
            &task.id,
            PlanningAction::Assign {
                agent_id: "helper".into(),
            },
        );
        change(
            &planner,
            &task.id,
            PlanningAction::Schedule {
                start_after: Some("2100-01-01T00:00:00Z".into()),
                activate: false,
            },
        );
        assert!(!planner.get(&task.id).unwrap().planning.queued);
        // Even a user message/retry that explicitly enqueues work cannot run it.
        let queued = queue
            .enqueue_continuation(&task.id, "helper")
            .unwrap()
            .unwrap();
        assert!(queue
            .claim_at(None, time("2101-01-01T00:00:00Z"))
            .unwrap()
            .is_none());
        drop((board, queue, planner));
        let (board, queue, planner) = setup(Arc::new(Database::open(&path).unwrap()));
        queue.recover_in_progress().unwrap();
        assert!(board.get(&task.id).unwrap().backlog);
        assert!(queue
            .claim_at(None, time("2101-01-01T00:00:00Z"))
            .unwrap()
            .is_none());
        let promoted = change(&planner, &task.id, PlanningAction::Queue);
        assert!(!promoted.task.backlog);
        assert_eq!(
            promoted.task.start_after.as_deref(),
            Some("2100-01-01T00:00:00.000Z")
        );
        assert!(queue
            .claim_at(None, time("2099-12-31T23:59:59Z"))
            .unwrap()
            .is_none());
        assert_eq!(
            queue
                .claim_at(None, time("2100-01-01T00:00:00Z"))
                .unwrap()
                .unwrap()
                .id,
            queued.id
        );
        assert!(queue
            .claim_at(None, time("2101-01-01T00:00:00Z"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn scheduling_can_explicitly_promote_backlog_but_cannot_override_review_or_input() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (board, queue, planner) = setup(db.clone());
        let task = board
            .create(&CreateTask {
                title: "Schedule a backlog idea".into(),
                agent_id: Some("atlas".into()),
                backlog: true,
                ..Default::default()
            })
            .unwrap();
        let scheduled = change(
            &planner,
            &task.id,
            PlanningAction::Schedule {
                start_after: Some("2100-01-01T00:00:00Z".into()),
                activate: true,
            },
        );
        assert_eq!(scheduled.planning.lane, "scheduled");
        assert!(!scheduled.task.backlog);
        assert!(scheduled.planning.queued);
        let item = queue
            .list(Some("atlas"), Some("queued"), 10)
            .unwrap()
            .remove(0);
        // Attempt state is authoritative even before the task's status catches up.
        for (state, lane) in [("review", "review"), ("waiting_for_input", "input")] {
            db.with_conn(|conn| {
                conn.execute(
                    "UPDATE work_attempts SET status=?1 WHERE id=?2",
                    params![state, item.attempt_id],
                )
            })
            .unwrap();
            let snapshot = planner.get(&task.id).unwrap();
            assert_eq!(snapshot.planning.lane, lane);
            assert!(snapshot.planning.disabled_reason.is_some());
            assert!(planner
                .change(
                    &task.id,
                    &PlanningChange {
                        expected_revision: snapshot.task.revision,
                        change: PlanningAction::Backlog
                    }
                )
                .is_err());
        }
    }
    #[test]
    fn edits_holds_cancel_and_concurrent_claims_do_not_start_early() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (board, queue, planner) = setup(db.clone());
        let task = create(&board, "Edit", None, 0);
        let stale = planner.get(&task.id).unwrap().task.revision;
        change(
            &planner,
            &task.id,
            PlanningAction::Schedule {
                activate: false,
                start_after: Some("2100-01-01T00:00:00Z".into()),
            },
        );
        assert!(planner
            .change(
                &task.id,
                &PlanningChange {
                    expected_revision: stale,
                    change: PlanningAction::Queue
                }
            )
            .is_err());
        assert!(queue.claim_next().unwrap().is_none());
        change(&planner, &task.id, PlanningAction::Backlog);
        change(
            &planner,
            &task.id,
            PlanningAction::Schedule {
                start_after: None,
                activate: false,
            },
        );
        assert!(queue.claim_next().unwrap().is_none());
        change(&planner, &task.id, PlanningAction::Queue);
        let queued = queue.claim_next().unwrap().unwrap();
        let revision = planner.get(&task.id).unwrap().task.revision;
        assert!(planner
            .change(
                &task.id,
                &PlanningChange {
                    expected_revision: revision,
                    change: PlanningAction::Backlog
                }
            )
            .is_err());
        assert_eq!(queue.get(queued.id).unwrap().status, "running");
        let cancelled = create(&board, "Cancel future", Some("2100-01-01T00:00:00Z"), 0);
        queue.enqueue(&cancelled.id, "helper").unwrap();
        board
            .update_status(&cancelled.id, "cancelled", None)
            .unwrap();
        assert!(queue
            .claim_at(Some("helper"), time("2101-01-01T00:00:00Z"))
            .unwrap()
            .is_none());
        let revision = board.get(&cancelled.id).unwrap().revision;
        assert!(planner
            .change(
                &cancelled.id,
                &PlanningChange {
                    expected_revision: revision,
                    change: PlanningAction::Schedule {
                        start_after: None,
                        activate: false
                    }
                }
            )
            .is_err());
    }
    #[test]
    fn reordering_tied_positions_preserves_other_order_in_both_directions() {
        // Import order and id order can differ. Ranking must use one snapshot,
        // even while the UPDATE changes the indexed positions underneath it.
        for reverse_import in [false, true] {
            for target in ["c", "d"] {
                for before in [false, true] {
                    let db = Arc::new(Database::open_memory().unwrap());
                    let (_, _, planner) = setup(db.clone());
                    let mut imported =
                        vec![("a", 1024), ("b", 2048), ("c", 1), ("d", 1), ("e", 3072)];
                    if reverse_import {
                        imported.reverse();
                    }
                    db.with_conn(|conn| {
                        for (id, position) in imported {
                            conn.execute("INSERT INTO tasks(id, title, position) VALUES(?1, ?1, ?2)", params![id, position])?;
                        }
                        conn.execute("INSERT INTO tasks(id, title, priority, position) VALUES('other-priority', 'Other priority', 10, 1)", [])
                    }).unwrap();
                    change(
                        &planner,
                        "e",
                        PlanningAction::Reorder {
                            before_id: before.then(|| target.into()),
                            after_id: (!before).then(|| target.into()),
                        },
                    );
                    let page = planner.list(&PlanningFilter::default()).unwrap();
                    let tasks: Vec<_> =
                        page.tasks.iter().filter(|t| t.task.priority == 0).collect();
                    assert!(tasks.windows(2).all(|pair| pair[0].task.position < pair[1].task.position), "Rebalance must leave unique positions (reverse import: {reverse_import}, target: {target}, before: {before})");
                    let mut expected = vec!["c", "d", "a", "b"];
                    let index = expected.iter().position(|id| *id == target).unwrap();
                    expected.insert(index + usize::from(!before), "e");
                    assert_eq!(
                        tasks.iter().map(|t| t.task.id.as_str()).collect::<Vec<_>>(),
                        expected
                    );
                    let untouched = planner.get("other-priority").unwrap().task;
                    assert_eq!(untouched.position, 1.0);
                    assert_eq!(untouched.revision, 0);
                }
            }
        }
    }
    #[test]
    fn dependencies_order_assignment_and_project_state_still_gate_dispatch() {
        let db = Arc::new(Database::open_memory().unwrap());
        let (board, queue, planner) = setup(db.clone());
        let parent = create(&board, "Dependency", None, 0);
        let a = create(&board, "A", Some("2000-01-01T00:00:00Z"), 0);
        let b = create(&board, "B", None, 0);
        board.add_dependency(&a.id, &parent.id).unwrap();
        queue.enqueue(&a.id, "atlas").unwrap();
        assert!(queue.claim_next().unwrap().is_none());
        queue.enqueue(&b.id, "atlas").unwrap();
        change(
            &planner,
            &b.id,
            PlanningAction::Assign {
                agent_id: "helper".into(),
            },
        );
        assert_eq!(queue.claim_next().unwrap().unwrap().agent_id, "helper");
        board.update_status(&parent.id, "completed", None).unwrap();
        assert_eq!(queue.claim_next().unwrap().unwrap().task_id, a.id);
        let c = create(&board, "C", None, 0);
        let d = create(&board, "D", None, 0);
        let neighbor_revision = c.revision;
        change(
            &planner,
            &d.id,
            PlanningAction::Reorder {
                before_id: Some(c.id.clone()),
                after_id: None,
            },
        );
        assert!(board.get(&d.id).unwrap().position < board.get(&c.id).unwrap().position);
        assert_eq!(board.get(&c.id).unwrap().revision, neighbor_revision);
        // Imported snapshots may contain ties; ordering repairs them only when needed.
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE tasks SET position=1 WHERE id IN (?1,?2)",
                params![c.id, d.id],
            )
        })
        .unwrap();
        let e = create(&board, "E", None, 0);
        change(
            &planner,
            &e.id,
            PlanningAction::Reorder {
                before_id: Some(c.id.clone()),
                after_id: None,
            },
        );
        assert!(board.get(&e.id).unwrap().position < board.get(&c.id).unwrap().position);
        assert!(normalize_start_after(Some("2026-09-10T09:30")).is_err());
        assert_eq!(
            normalize_start_after(Some("2100-01-01T00:00:00.0001Z"))
                .unwrap()
                .as_deref(),
            Some("2100-01-01T00:00:00.001Z")
        );
        db.with_conn(|conn| conn.execute_batch("INSERT INTO projects(id,name,deletion_started_at) VALUES('deleting','Deleting',CURRENT_TIMESTAMP); UPDATE tasks SET project_id = 'deleting' WHERE title = 'C';")).unwrap();
        assert!(queue.enqueue(&c.id, "atlas").is_err());
    }
}

#[cfg(test)]
mod race_tests {
    use super::super::board::{CreateTask, TaskBoard};
    use super::*;
    use crate::agents::registry::AgentRegistry;
    #[test]
    fn two_connections_claim_the_due_task_only_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("race.db");
        let first = Arc::new(Database::open(&path).unwrap());
        AgentRegistry::new(first.clone())
            .ensure("atlas", "native")
            .unwrap();
        let task = TaskBoard::new(first.clone())
            .create(&CreateTask {
                title: "Only once".into(),
                agent_id: Some("atlas".into()),
                start_after: Some("2100-01-01T00:00:00Z".into()),
                ..Default::default()
            })
            .unwrap();
        TaskQueue::new(first.clone())
            .enqueue(&task.id, "atlas")
            .unwrap();
        let second = Arc::new(Database::open(&path).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [first, second]
            .into_iter()
            .map(|db| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    TaskQueue::new(db)
                        .claim_at(None, "2100-01-01T00:00:00Z".parse().unwrap())
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|handle| handle.join().unwrap().then_some(1))
                .sum::<i32>(),
            1
        );
    }
}
