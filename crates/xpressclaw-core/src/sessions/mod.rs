//! Event-driven logical sessions and their isolated native work attempts.
//!
//! A session is the durable task context for one Agent. Native harnesses own
//! their instructions and subagents; every invocation is represented by a work
//! attempt that contributes structured events and artifacts to the timeline.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::projects::ensure_project_accepts_work;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalSession {
    pub id: String,
    pub agent_id: String,
    pub title: Option<String>,
    pub status: String,
    pub latest_summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkAttempt {
    pub id: String,
    pub session_id: String,
    pub task_id: Option<String>,
    pub queue_id: Option<i64>,
    pub kind: String,
    pub runner: String,
    pub status: String,
    pub prompt: String,
    pub native_session_id: Option<String>,
    pub container_id: Option<String>,
    pub result: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub context_used: Option<i64>,
    pub context_size: Option<i64>,
    /// User message that triggered this response cycle, when applicable.
    pub trigger_message_id: Option<i64>,
    /// UTC timestamp at which this response cycle entered the queue.
    pub response_queued_at: Option<String>,
    /// UTC timestamp at which the agent began the active response phase.
    pub response_started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub id: i64,
    pub session_id: String,
    pub attempt_id: Option<String>,
    pub task_id: Option<String>,
    pub source_type: String,
    pub source_id: Option<String>,
    pub event_type: String,
    pub summary: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptArtifact {
    pub id: String,
    pub attempt_id: String,
    pub session_id: String,
    pub artifact_type: String,
    pub title: String,
    pub content: Option<String>,
    pub uri: Option<String>,
    pub metadata: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOverview {
    pub session: LogicalSession,
    pub active_attempts: Vec<WorkAttempt>,
    pub queued_attempts: Vec<WorkAttempt>,
    pub recent_attempts: Vec<WorkAttempt>,
    pub recent_events: Vec<SessionEvent>,
    pub artifacts: Vec<AttemptArtifact>,
}

/// Native work history scoped to one task. This is the task page's semantic
/// activity feed: attempts and structured events, never raw terminal output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskActivity {
    pub attempts: Vec<WorkAttempt>,
    pub events: Vec<SessionEvent>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    pub attempt_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub source_type: &'a str,
    pub source_id: Option<&'a str>,
    pub event_type: &'a str,
    pub summary: &'a str,
    pub payload: Value,
}

pub struct SessionManager {
    db: Arc<Database>,
}

impl SessionManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Ensure that the durable Agent task session exists. IDs retain the
    /// legacy registry key shape so existing task and workflow references stay
    /// valid, but titles are always refreshed from the current project path.
    pub fn ensure(&self, session_id: &str, title: Option<&str>) -> Result<LogicalSession> {
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let project_id = transaction
                .query_row(
                    "SELECT project_id FROM agents WHERE id = ?1",
                    [session_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::AgentNotFound {
                    name: session_id.to_string(),
                })?;
            if let Some(project_id) = project_id.as_deref() {
                ensure_project_accepts_work(&transaction, project_id)?;
            }
            transaction.execute(
                "INSERT OR IGNORE INTO logical_sessions (id, agent_id, title) VALUES (?1, ?1, ?2)",
                rusqlite::params![session_id, title],
            )?;
            if title.is_some() {
                transaction.execute(
                    "UPDATE logical_sessions SET title = ?1 WHERE id = ?2",
                    rusqlite::params![title, session_id],
                )?;
            }
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;
        self.get(session_id)
    }

    pub fn get(&self, session_id: &str) -> Result<LogicalSession> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, agent_id, title, status, latest_summary, created_at, updated_at
                 FROM logical_sessions WHERE id = ?1",
                [session_id],
                row_to_session,
            )
            .map_err(|_| Error::AgentNotFound {
                name: session_id.to_string(),
            })
        })
    }

    pub fn overview(&self, session_id: &str) -> Result<SessionOverview> {
        let session = self.get(session_id)?;
        Ok(SessionOverview {
            active_attempts: self.list_attempts(session_id, Some("running"), 20)?,
            queued_attempts: self.list_attempts(session_id, Some("queued"), 20)?,
            recent_attempts: self.list_attempts(session_id, None, 30)?,
            recent_events: self.list_events(session_id, None, 100)?,
            artifacts: self.list_artifacts(session_id, 50)?,
            session,
        })
    }

    pub fn append_event(&self, session_id: &str, event: NewEvent<'_>) -> Result<SessionEvent> {
        let payload = serde_json::to_string(&event.payload)?;
        let id = self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO session_events
                 (session_id, attempt_id, task_id, source_type, source_id, event_type, summary, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    session_id,
                    event.attempt_id,
                    event.task_id,
                    event.source_type,
                    event.source_id,
                    event.event_type,
                    event.summary,
                    payload,
                ],
            )?;
            conn.execute(
                "UPDATE logical_sessions
                 SET latest_summary = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
                rusqlite::params![event.summary, session_id],
            )?;
            Ok::<_, Error>(conn.last_insert_rowid())
        })?;
        self.get_event(id)
    }

    pub fn get_event(&self, id: i64) -> Result<SessionEvent> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, session_id, attempt_id, task_id, source_type, source_id,
                        event_type, summary, payload, created_at
                 FROM session_events WHERE id = ?1",
                [id],
                row_to_event,
            )
            .map_err(|e| Error::Database(e.to_string()))
        })
    }

    pub fn list_events(
        &self,
        session_id: &str,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<SessionEvent>> {
        self.db.with_conn(|conn| {
            let (sql, pivot) = if let Some(after_id) = after {
                (
                    "SELECT id, session_id, attempt_id, task_id, source_type, source_id,
                            event_type, summary, payload, created_at
                     FROM session_events WHERE session_id = ?1 AND id > ?2
                     ORDER BY id ASC LIMIT ?3",
                    after_id,
                )
            } else {
                (
                    "SELECT id, session_id, attempt_id, task_id, source_type, source_id,
                            event_type, summary, payload, created_at
                     FROM session_events WHERE session_id = ?1 AND id > ?2
                     ORDER BY id DESC LIMIT ?3",
                    -1,
                )
            };
            let mut stmt = conn.prepare(sql)?;
            let mut events: Vec<_> = stmt
                .query_map(rusqlite::params![session_id, pivot, limit], row_to_event)?
                .filter_map(|row| row.ok())
                .collect();
            if after.is_none() {
                events.reverse();
            }
            Ok(events)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_attempt(
        &self,
        session_id: &str,
        task_id: &str,
        queue_id: i64,
        runner: &str,
        kind: &str,
        source_type: &str,
        source_id: Option<&str>,
        prompt: &str,
    ) -> Result<WorkAttempt> {
        let id = Uuid::new_v4().to_string();
        self.db.with_conn(|conn| {
            let transaction = rusqlite::Transaction::new_unchecked(
                conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let project_id = transaction
                .query_row(
                    "SELECT project_id FROM tasks WHERE id = ?1",
                    [task_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or_else(|| Error::TaskNotFound {
                    id: task_id.to_string(),
                })?;
            if let Some(project_id) = project_id.as_deref() {
                ensure_project_accepts_work(&transaction, project_id)?;
            }
            transaction.execute(
                "INSERT OR IGNORE INTO logical_sessions (id, agent_id, title)
                 VALUES (?1, ?1, NULL)",
                [session_id],
            )?;
            transaction.execute(
                "INSERT INTO work_attempts
                 (id, session_id, task_id, queue_id, kind, runner, status, prompt,
                  response_queued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, CURRENT_TIMESTAMP)",
                rusqlite::params![id, session_id, task_id, queue_id, kind, runner, prompt],
            )?;
            transaction.execute(
                "UPDATE task_queue SET attempt_id = ?1 WHERE id = ?2",
                rusqlite::params![id, queue_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET session_id = ?1, active_attempt_id = ?2,
                    updated_at = CURRENT_TIMESTAMP WHERE id = ?3",
                rusqlite::params![session_id, id, task_id],
            )?;
            transaction.execute(
                "UPDATE logical_sessions SET status = 'queued', updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND status != 'running'",
                [session_id],
            )?;
            transaction.commit()?;
            Ok::<_, Error>(())
        })?;
        self.append_event(
            session_id,
            NewEvent {
                attempt_id: Some(&id),
                task_id: Some(task_id),
                source_type,
                source_id: source_id.or(Some(task_id)),
                event_type: "attempt_queued",
                summary: "Work queued",
                payload: json!({ "runner": runner, "kind": kind, "queue_id": queue_id }),
            },
        )?;
        self.get_attempt(&id)
    }

    pub fn get_attempt(&self, attempt_id: &str) -> Result<WorkAttempt> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, session_id, task_id, queue_id, kind, runner, status, prompt,
                        native_session_id, container_id, result, error_message,
                        created_at, started_at, completed_at, context_used, context_size,
                        trigger_message_id, response_queued_at, response_started_at
                 FROM work_attempts WHERE id = ?1",
                [attempt_id],
                row_to_attempt,
            )
            .map_err(|e| Error::Task(format!("attempt {attempt_id} not found: {e}")))
        })
    }

    pub fn list_attempts(
        &self,
        session_id: &str,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WorkAttempt>> {
        self.db.with_conn(|conn| {
            let mut sql = String::from(
                "SELECT id, session_id, task_id, queue_id, kind, runner, status, prompt,
                        native_session_id, container_id, result, error_message,
                        created_at, started_at, completed_at, context_used, context_size,
                        trigger_message_id, response_queued_at, response_started_at
                 FROM work_attempts WHERE session_id = ?1",
            );
            let mut values: Vec<Box<dyn rusqlite::types::ToSql>> =
                vec![Box::new(session_id.to_string())];
            if let Some(value) = status {
                sql.push_str(" AND status = ?2");
                values.push(Box::new(value.to_string()));
            }
            sql.push_str(if status.is_some() {
                " ORDER BY created_at DESC LIMIT ?3"
            } else {
                " ORDER BY created_at DESC LIMIT ?2"
            });
            values.push(Box::new(limit));
            let refs: Vec<&dyn rusqlite::types::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            let mut stmt = conn.prepare(&sql)?;
            let attempts = stmt
                .query_map(refs.as_slice(), row_to_attempt)?
                .filter_map(|row| row.ok())
                .collect();
            Ok(attempts)
        })
    }

    pub fn task_activity(
        &self,
        task_id: &str,
        after: Option<i64>,
        before: Option<i64>,
        event_limit: i64,
        attempt_limit: i64,
    ) -> Result<TaskActivity> {
        if after.is_some() && before.is_some() {
            return Err(Error::Task(
                "task activity accepts either 'after' or 'before', not both".to_string(),
            ));
        }
        self.db.with_conn(|conn| {
            let mut attempt_stmt = conn.prepare(
                "SELECT id, session_id, task_id, queue_id, kind, runner, status, prompt,
                        native_session_id, container_id, result, error_message,
                        created_at, started_at, completed_at, context_used, context_size,
                        trigger_message_id, response_queued_at, response_started_at
                 FROM work_attempts WHERE task_id = ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let attempts = attempt_stmt
                .query_map(rusqlite::params![task_id, attempt_limit], row_to_attempt)?
                .filter_map(|row| row.ok())
                .collect();

            let (event_sql, pivot, descending) = if let Some(after_id) = after {
                (
                    "SELECT id, session_id, attempt_id, task_id, source_type, source_id,
                            event_type, summary, payload, created_at
                     FROM session_events WHERE task_id = ?1 AND id > ?2
                     ORDER BY id ASC LIMIT ?3",
                    after_id,
                    false,
                )
            } else if let Some(before_id) = before {
                (
                    "SELECT id, session_id, attempt_id, task_id, source_type, source_id,
                            event_type, summary, payload, created_at
                     FROM session_events WHERE task_id = ?1 AND id < ?2
                     ORDER BY id DESC LIMIT ?3",
                    before_id,
                    true,
                )
            } else {
                (
                    "SELECT id, session_id, attempt_id, task_id, source_type, source_id,
                            event_type, summary, payload, created_at
                    FROM session_events WHERE task_id = ?1 AND id > ?2
                     ORDER BY id DESC LIMIT ?3",
                    -1,
                    true,
                )
            };
            let mut event_stmt = conn.prepare(event_sql)?;
            let fetch_limit = event_limit.saturating_add(1);
            let mut events: Vec<_> = event_stmt
                .query_map(rusqlite::params![task_id, pivot, fetch_limit], row_to_event)?
                .filter_map(|row| row.ok())
                .collect();
            let has_more = events.len() > event_limit as usize;
            if has_more {
                events.truncate(event_limit as usize);
            }
            if descending {
                events.reverse();
            }

            Ok(TaskActivity {
                attempts,
                events,
                has_more_before: descending && has_more,
                has_more_after: !descending && has_more,
            })
        })
    }

    pub fn transition_attempt(
        &self,
        attempt_id: &str,
        status: &str,
        summary: &str,
        result: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<WorkAttempt> {
        const VALID: &[&str] = &[
            "queued",
            "preparing",
            "running",
            "waiting_for_input",
            "review",
            "completed",
            "failed",
            "cancelled",
            "interrupted",
        ];
        if !VALID.contains(&status) {
            return Err(Error::Task(format!("invalid attempt status: {status}")));
        }
        let attempt = self.get_attempt(attempt_id)?;
        let is_terminal = matches!(status, "completed" | "failed" | "cancelled" | "interrupted");
        let transitioned = self.db.with_conn(|conn| {
            let updated = conn.execute(
                "UPDATE work_attempts SET status = ?1,
                    started_at = CASE WHEN ?1 IN ('preparing', 'running')
                        THEN COALESCE(started_at, CURRENT_TIMESTAMP) ELSE started_at END,
                    response_started_at = CASE
                        WHEN ?1 = 'running' AND status != 'running' THEN CURRENT_TIMESTAMP
                        WHEN ?1 = 'running' THEN COALESCE(response_started_at, CURRENT_TIMESTAMP)
                        ELSE response_started_at END,
                    completed_at = CASE WHEN ?2 = 1 THEN CURRENT_TIMESTAMP ELSE completed_at END,
                    result = COALESCE(?3, result), error_message = ?4
                 WHERE id = ?5
                   AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')",
                rusqlite::params![
                    status,
                    is_terminal as i32,
                    result,
                    error_message,
                    attempt_id
                ],
            )?;
            if updated == 0 {
                return Ok::<_, Error>(false);
            }
            let session_status = self.derive_status_with_conn(conn, &attempt.session_id)?;
            conn.execute(
                "UPDATE logical_sessions SET status = ?1, latest_summary = ?2,
                    updated_at = CURRENT_TIMESTAMP WHERE id = ?3",
                rusqlite::params![session_status, summary, attempt.session_id],
            )?;
            if is_terminal {
                conn.execute(
                    "UPDATE tasks SET active_attempt_id = NULL, updated_at = CURRENT_TIMESTAMP
                     WHERE id = ?1 AND active_attempt_id = ?2",
                    rusqlite::params![attempt.task_id, attempt_id],
                )?;
            }
            Ok::<_, Error>(true)
        })?;
        if !transitioned {
            return self.get_attempt(attempt_id);
        }
        self.append_event(
            &attempt.session_id,
            NewEvent {
                attempt_id: Some(attempt_id),
                task_id: attempt.task_id.as_deref(),
                source_type: "runner",
                source_id: Some(&attempt.runner),
                event_type: &format!("attempt_{status}"),
                summary,
                payload: json!({ "status": status, "error": error_message }),
            },
        )?;
        self.get_attempt(attempt_id)
    }

    /// Associate a queued continuation with the latest user message it will
    /// answer. Updating an already-queued attempt is intentional: consecutive
    /// guidance is coalesced into one response cycle whose queue latency starts
    /// at the newest message included in that response.
    pub fn associate_response_trigger(
        &self,
        attempt_id: &str,
        message_id: i64,
        queued_at: &str,
    ) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts
                 SET trigger_message_id = ?1, response_queued_at = ?2
                 WHERE id = ?3
                   AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')
                   AND (trigger_message_id IS NULL OR trigger_message_id <= ?1)",
                rusqlite::params![message_id, queued_at, attempt_id],
            )?;
            Ok(())
        })
    }

    /// Cancel every live attempt for a task before any asynchronous container
    /// cleanup begins. Selecting the attempts and transitioning the attempts,
    /// queued dispatches, task, active-attempt pointer, logical sessions, and
    /// pull-request review gates happen together so work enqueued immediately
    /// before cancellation cannot survive it.
    ///
    /// `None` means the task already completed in another path, so the caller
    /// must preserve that terminal state. Otherwise the returned attempts are
    /// the ones whose containers and running dispatch leases need cleanup.
    pub fn cancel_task_attempts(
        &self,
        task_id: &str,
        summary: &str,
    ) -> Result<Option<Vec<WorkAttempt>>> {
        let attempts = self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let attempts =
                self.cancel_task_attempts_in_transaction(&transaction, task_id, summary)?;
            transaction.commit()?;
            Ok::<_, Error>(attempts)
        })?;

        attempts
            .map(|attempts| self.record_task_attempt_cancellations(task_id, summary, attempts))
            .transpose()
    }

    /// Apply the durable half of whole-task cancellation inside a caller-owned
    /// transaction.
    pub(crate) fn cancel_task_attempts_in_transaction(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        task_id: &str,
        summary: &str,
    ) -> Result<Option<Vec<WorkAttempt>>> {
        let task_status: String =
            transaction.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })?;
        if task_status == "completed" {
            return Ok(None);
        }

        let mut statement = transaction.prepare(
            "SELECT id, session_id, task_id, queue_id, kind, runner, status, prompt,
                        native_session_id, container_id, result, error_message,
                        created_at, started_at, completed_at, context_used, context_size,
                        trigger_message_id, response_queued_at, response_started_at
                 FROM work_attempts
                 WHERE task_id = ?1
                   AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')
                 ORDER BY created_at ASC",
        )?;
        let attempts = statement
            .query_map([task_id], row_to_attempt)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);

        transaction.execute(
            "UPDATE work_attempts SET status = 'cancelled',
                    completed_at = CURRENT_TIMESTAMP, error_message = NULL
                 WHERE task_id = ?1
                   AND status NOT IN ('completed', 'failed', 'cancelled', 'interrupted')",
            [task_id],
        )?;
        // Queued dispatches are safe to release immediately because their
        // attempts are now terminal. Running rows remain the retained
        // container lease until the caller finishes asynchronous cleanup.
        transaction.execute(
            "UPDATE task_queue SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                    harness_response = 'cancelled by user'
                 WHERE task_id = ?1 AND status = 'queued'",
            [task_id],
        )?;
        transaction.execute(
            "UPDATE tasks SET status = 'cancelled', completed_at = NULL,
                    active_attempt_id = NULL,
                    updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND status != 'completed'",
            [task_id],
        )?;
        transaction.execute(
            "UPDATE task_pull_requests SET status = 'cancelled', next_poll_at = NULL,
                    last_checked_at = CURRENT_TIMESTAMP, last_error = NULL
                WHERE task_id = ?1 AND status IN ('waiting', 'attention')",
            [task_id],
        )?;
        let mut session_ids = attempts
            .iter()
            .map(|attempt| attempt.session_id.as_str())
            .collect::<Vec<_>>();
        session_ids.sort_unstable();
        session_ids.dedup();
        for session_id in session_ids {
            let session_status = self.derive_status_with_conn(transaction, session_id)?;
            transaction.execute(
                "UPDATE logical_sessions SET status = ?1, latest_summary = ?2,
                        updated_at = CURRENT_TIMESTAMP WHERE id = ?3",
                rusqlite::params![session_status, summary, session_id],
            )?;
        }
        Ok(Some(attempts))
    }

    /// Cancel one workflow-owned attempt without changing unrelated attempts
    /// or making the shared source task terminal. Same-task continuations use
    /// this narrower lifecycle when their workflow run is cancelled.
    pub(crate) fn cancel_workflow_attempt_in_transaction(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        attempt_id: &str,
        summary: &str,
    ) -> Result<Option<WorkAttempt>> {
        let attempt = transaction
            .query_row(
                "SELECT id, session_id, task_id, queue_id, kind, runner, status, prompt,
                        native_session_id, container_id, result, error_message,
                        created_at, started_at, completed_at, context_used, context_size,
                        trigger_message_id, response_queued_at, response_started_at
                 FROM work_attempts WHERE id = ?1",
                [attempt_id],
                row_to_attempt,
            )
            .optional()?
            .ok_or_else(|| Error::Task(format!("attempt {attempt_id} not found")))?;
        if matches!(
            attempt.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        ) {
            return Ok(None);
        }
        let task_id = attempt.task_id.as_deref().ok_or_else(|| {
            Error::Task(format!(
                "workflow continuation attempt {attempt_id} has no task"
            ))
        })?;

        transaction.execute(
            "UPDATE work_attempts
             SET status = 'cancelled', completed_at = CURRENT_TIMESTAMP,
                 error_message = NULL
             WHERE id = ?1",
            [attempt_id],
        )?;
        if let Some(queue_id) = attempt.queue_id {
            // A running dispatch retains the container lease until the server
            // has stopped it. A queued dispatch can be retired immediately.
            transaction.execute(
                "UPDATE task_queue
                 SET status = 'failed', completed_at = CURRENT_TIMESTAMP,
                     harness_response = 'workflow cancelled by user'
                 WHERE id = ?1 AND status = 'queued'",
                [queue_id],
            )?;
        }
        if attempt.status == "queued" {
            if let Some(message_id) = attempt.trigger_message_id {
                // Same-task workflow continuations insert their own fixed
                // user message and own the resulting attempt exclusively.
                // If that queued continuation is cancelled, remove its
                // unconsumed prompt as part of the same transaction; leaving
                // it in task history would make a later user turn execute it.
                // The extra ownership guard preserves a message if another
                // live attempt ever references it despite that invariant.
                transaction.execute(
                    "DELETE FROM task_messages
                     WHERE id = ?1 AND task_id = ?2 AND role = 'user'
                       AND NOT EXISTS (
                           SELECT 1 FROM work_attempts
                           WHERE trigger_message_id = ?1 AND id != ?3
                             AND status IN ('queued', 'preparing', 'running',
                                            'waiting_for_input', 'review')
                       )",
                    rusqlite::params![message_id, task_id, attempt_id],
                )?;
            }
        }

        let replacement_attempt_id = transaction
            .query_row(
                "SELECT id FROM work_attempts
                 WHERE task_id = ?1
                   AND status IN ('queued', 'preparing', 'running',
                                  'waiting_for_input', 'review')
                 ORDER BY rowid DESC LIMIT 1",
                [task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let current_task_status: String =
            transaction.query_row("SELECT status FROM tasks WHERE id = ?1", [task_id], |row| {
                row.get(0)
            })?;
        let next_task_status = if matches!(current_task_status.as_str(), "completed" | "cancelled")
            || replacement_attempt_id.is_some()
        {
            current_task_status
        } else {
            "completed".to_string()
        };
        transaction.execute(
            "UPDATE tasks
             SET status = ?1, active_attempt_id = ?2,
                 completed_at = CASE
                     WHEN ?1 = 'completed' THEN COALESCE(completed_at, CURRENT_TIMESTAMP)
                     ELSE NULL END,
                 updated_at = CURRENT_TIMESTAMP
             WHERE id = ?3",
            rusqlite::params![next_task_status, replacement_attempt_id, task_id],
        )?;

        let session_status = self.derive_status_with_conn(transaction, &attempt.session_id)?;
        transaction.execute(
            "UPDATE logical_sessions
             SET status = ?1, latest_summary = ?2, updated_at = CURRENT_TIMESTAMP
             WHERE id = ?3",
            rusqlite::params![session_status, summary, &attempt.session_id],
        )?;
        Ok(Some(attempt))
    }

    pub(crate) fn record_task_attempt_cancellations(
        &self,
        task_id: &str,
        summary: &str,
        attempts: Vec<WorkAttempt>,
    ) -> Result<Vec<WorkAttempt>> {
        let mut cancelled = Vec::with_capacity(attempts.len());
        for attempt in attempts {
            self.append_event(
                &attempt.session_id,
                NewEvent {
                    attempt_id: Some(&attempt.id),
                    task_id: Some(task_id),
                    source_type: "runner",
                    source_id: Some(&attempt.runner),
                    event_type: "attempt_cancelled",
                    summary,
                    payload: json!({ "status": "cancelled", "error": Value::Null }),
                },
            )?;
            cancelled.push(self.get_attempt(&attempt.id)?);
        }
        Ok(cancelled)
    }

    pub fn set_container(&self, attempt_id: &str, container_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET container_id = ?1 WHERE id = ?2",
                rusqlite::params![container_id, attempt_id],
            )?;
            Ok(())
        })
    }

    /// Release this attempt's lease on its Agent container after the
    /// process has stopped. The container itself remains available for a
    /// later attempt.
    pub fn clear_container(&self, attempt_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET container_id = NULL WHERE id = ?1",
                [attempt_id],
            )?;
            Ok(())
        })
    }

    pub fn set_native_session(&self, attempt_id: &str, native_session_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts SET native_session_id = ?1 WHERE id = ?2",
                rusqlite::params![native_session_id, attempt_id],
            )?;
            Ok(())
        })
    }

    /// Store the latest ACP context-window counters without adding a noisy
    /// timeline event. Task activity polling returns attempts on every request,
    /// so clients still receive live usage updates while a turn is running.
    pub fn set_context_usage(&self, attempt_id: &str, used: u64, size: u64) -> Result<()> {
        let used = i64::try_from(used)
            .map_err(|_| Error::Backend("ACP context usage exceeds SQLite range".to_string()))?;
        let size = i64::try_from(size)
            .map_err(|_| Error::Backend("ACP context size exceeds SQLite range".to_string()))?;
        self.db.with_conn(|conn| {
            let updated = conn.execute(
                "UPDATE work_attempts SET context_used = ?1, context_size = ?2 WHERE id = ?3",
                rusqlite::params![used, size, attempt_id],
            )?;
            if updated == 0 {
                return Err(Error::Task(format!("attempt {attempt_id} not found")));
            }
            Ok(())
        })
    }

    /// Recompute the project-facing status after task state changes that do
    /// not themselves transition a work attempt (notably waiting for input).
    pub fn refresh_status(&self, session_id: &str) -> Result<LogicalSession> {
        self.db.with_conn(|conn| {
            let status = self.derive_status_with_conn(conn, session_id)?;
            conn.execute(
                "UPDATE logical_sessions SET status = ?1, updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?2",
                rusqlite::params![status, session_id],
            )?;
            Ok::<_, Error>(())
        })?;
        self.get(session_id)
    }

    pub fn add_artifact(
        &self,
        attempt_id: &str,
        artifact_type: &str,
        title: &str,
        content: Option<&str>,
        uri: Option<&str>,
        metadata: Value,
    ) -> Result<AttemptArtifact> {
        let attempt = self.get_attempt(attempt_id)?;
        let id = Uuid::new_v4().to_string();
        let metadata_json = serde_json::to_string(&metadata)?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO attempt_artifacts
                 (id, attempt_id, session_id, artifact_type, title, content, uri, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    id,
                    attempt_id,
                    attempt.session_id,
                    artifact_type,
                    title,
                    content,
                    uri,
                    metadata_json,
                ],
            )?;
            Ok::<_, Error>(())
        })?;
        self.append_event(
            &attempt.session_id,
            NewEvent {
                attempt_id: Some(attempt_id),
                task_id: attempt.task_id.as_deref(),
                source_type: "runner",
                source_id: Some(&attempt.runner),
                event_type: "artifact_created",
                summary: title,
                payload: json!({ "artifact_id": id, "artifact_type": artifact_type, "uri": uri }),
            },
        )?;
        self.get_artifact(&id)
    }

    pub fn list_artifacts(&self, session_id: &str, limit: i64) -> Result<Vec<AttemptArtifact>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, attempt_id, session_id, artifact_type, title, content, uri,
                        metadata, created_at
                 FROM attempt_artifacts WHERE session_id = ?1
                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let artifacts = stmt
                .query_map(rusqlite::params![session_id, limit], row_to_artifact)?
                .filter_map(|row| row.ok())
                .collect();
            Ok(artifacts)
        })
    }

    /// Remove a logical session and its attempt/event/artifact history.
    /// Task and queue records are retained, but their session pointers are
    /// cleared before the cascading delete.
    pub fn delete(&self, session_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            tx.execute(
                "UPDATE tasks SET session_id = NULL, active_attempt_id = NULL
                 WHERE session_id = ?1",
                [session_id],
            )?;
            tx.execute(
                "UPDATE task_queue SET attempt_id = NULL WHERE agent_id = ?1",
                [session_id],
            )?;
            tx.execute("DELETE FROM logical_sessions WHERE id = ?1", [session_id])?;
            tx.commit()?;
            Ok(())
        })
    }

    fn get_artifact(&self, id: &str) -> Result<AttemptArtifact> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, attempt_id, session_id, artifact_type, title, content, uri,
                        metadata, created_at FROM attempt_artifacts WHERE id = ?1",
                [id],
                row_to_artifact,
            )
            .map_err(|e| Error::Database(e.to_string()))
        })
    }

    fn derive_status_with_conn(
        &self,
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> Result<String> {
        let running: i64 = conn.query_row(
            "SELECT COUNT(*) FROM work_attempts
             WHERE session_id = ?1 AND status IN ('preparing', 'running', 'review')",
            [session_id],
            |row| row.get(0),
        )?;
        if running > 0 {
            return Ok("running".to_string());
        }
        let queued: i64 = conn.query_row(
            "SELECT COUNT(*) FROM work_attempts
             WHERE session_id = ?1 AND status = 'queued'",
            [session_id],
            |row| row.get(0),
        )?;
        if queued > 0 {
            return Ok("queued".to_string());
        }
        let waiting: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE agent_id = ?1 AND status = 'waiting_for_input'",
            [session_id],
            |row| row.get(0),
        )?;
        if waiting > 0 {
            return Ok("waiting_for_input".to_string());
        }
        let blocked: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks
             WHERE agent_id = ?1 AND status = 'blocked'",
            [session_id],
            |row| row.get(0),
        )?;
        Ok(if blocked > 0 { "blocked" } else { "idle" }.to_string())
    }
}

fn parse_json(value: String) -> Value {
    serde_json::from_str(&value).unwrap_or_else(|_| json!({ "raw": value }))
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<LogicalSession> {
    Ok(LogicalSession {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        title: row.get(2)?,
        status: row.get(3)?,
        latest_summary: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn row_to_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkAttempt> {
    Ok(WorkAttempt {
        id: row.get(0)?,
        session_id: row.get(1)?,
        task_id: row.get(2)?,
        queue_id: row.get(3)?,
        kind: row.get(4)?,
        runner: row.get(5)?,
        status: row.get(6)?,
        prompt: row.get(7)?,
        native_session_id: row.get(8)?,
        container_id: row.get(9)?,
        result: row.get(10)?,
        error_message: row.get(11)?,
        created_at: row.get(12)?,
        started_at: row.get(13)?,
        completed_at: row.get(14)?,
        context_used: row.get(15)?,
        context_size: row.get(16)?,
        trigger_message_id: row.get(17)?,
        response_queued_at: row.get(18)?,
        response_started_at: row.get(19)?,
    })
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionEvent> {
    Ok(SessionEvent {
        id: row.get(0)?,
        session_id: row.get(1)?,
        attempt_id: row.get(2)?,
        task_id: row.get(3)?,
        source_type: row.get(4)?,
        source_id: row.get(5)?,
        event_type: row.get(6)?,
        summary: row.get(7)?,
        payload: parse_json(row.get(8)?),
        created_at: row.get(9)?,
    })
}

fn row_to_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttemptArtifact> {
    Ok(AttemptArtifact {
        id: row.get(0)?,
        attempt_id: row.get(1)?,
        session_id: row.get(2)?,
        artifact_type: row.get(3)?,
        title: row.get(4)?,
        content: row.get(5)?,
        uri: row.get(6)?,
        metadata: parse_json(row.get(7)?),
        created_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_agent(db: &Arc<Database>, id: &str) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO agents (id, name, backend, config)
                 VALUES (?1, ?1, 'native', '{}')",
                [id],
            )
        })
        .unwrap();
    }

    #[test]
    fn ensuring_a_session_serializes_with_project_deletion() {
        let db = Arc::new(Database::open_memory().unwrap());
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO projects (id, name) VALUES ('project-one', 'Project One');
                 INSERT INTO agents (id, name, backend, config, project_id)
                 VALUES ('atlas', 'Atlas', 'native', '{}', 'project-one');",
            )
        })
        .unwrap();
        let manager = SessionManager::new(db.clone());
        manager.ensure("atlas", Some("Before deletion")).unwrap();

        let projects = crate::projects::ProjectManager::new(db.clone());
        projects.begin_cascade("project-one").unwrap();
        let deleting_error = manager
            .ensure("atlas", Some("After deletion started"))
            .unwrap_err();
        assert!(deleting_error.to_string().contains("being deleted"));
        assert_eq!(
            manager.get("atlas").unwrap().title.as_deref(),
            Some("Before deletion")
        );

        projects.finish_cascade("project-one").unwrap();
        assert!(matches!(
            manager.ensure("atlas", Some("Stale request")),
            Err(Error::AgentNotFound { .. })
        ));
        let session_count = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM logical_sessions", [], |row| {
                    row.get::<_, i64>(0)
                })
            })
            .unwrap();
        assert_eq!(session_count, 0);
    }

    #[test]
    fn session_timeline_and_attempt_lifecycle() {
        let db = Arc::new(Database::open_memory().unwrap());
        insert_agent(&db, "builder");
        let manager = SessionManager::new(db.clone());
        manager.ensure("builder", Some("Builder")).unwrap();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO tasks (id, title) VALUES ('task-1', 'Build it')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO task_queue (id, task_id, agent_id, status) VALUES (1, 'task-1', 'builder', 'queued')",
                [],
            )
            .unwrap();
        });

        let attempt = manager
            .create_attempt(
                "builder",
                "task-1",
                1,
                "codex",
                "task",
                "task",
                Some("task-1"),
                "Build it",
            )
            .unwrap();
        assert_eq!(attempt.status, "queued");
        assert!(attempt.response_queued_at.is_some());
        assert!(attempt.response_started_at.is_none());

        let running = manager
            .transition_attempt(&attempt.id, "running", "Codex is working", None, None)
            .unwrap();
        assert!(running.response_started_at.is_some());
        manager
            .add_artifact(
                &attempt.id,
                "summary",
                "Implementation summary",
                Some("Done"),
                None,
                json!({}),
            )
            .unwrap();
        manager
            .transition_attempt(
                &attempt.id,
                "completed",
                "Implementation completed",
                Some("Done"),
                None,
            )
            .unwrap();

        let overview = manager.overview("builder").unwrap();
        assert_eq!(overview.session.status, "idle");
        assert!(overview.active_attempts.is_empty());
        assert_eq!(overview.artifacts.len(), 1);
        assert!(overview
            .recent_events
            .iter()
            .any(|event| event.event_type == "attempt_completed"));
    }

    #[test]
    fn response_phase_resets_after_waiting_without_rewriting_attempt_start() {
        let db = Arc::new(Database::open_memory().unwrap());
        insert_agent(&db, "builder");
        let manager = SessionManager::new(db.clone());
        manager.ensure("builder", Some("Builder")).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tasks (id, title) VALUES ('task-1', 'Build it')",
                [],
            )?;
            conn.execute(
                "INSERT INTO task_queue (id, task_id, agent_id, status)
                 VALUES (1, 'task-1', 'builder', 'queued')",
                [],
            )?;
            Ok::<_, Error>(())
        })
        .unwrap();
        let attempt = manager
            .create_attempt(
                "builder", "task-1", 1, "codex", "task", "task", None, "Build it",
            )
            .unwrap();
        manager
            .transition_attempt(&attempt.id, "running", "Responding", None, None)
            .unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE work_attempts
                 SET started_at = '2020-01-01 00:00:00',
                     response_started_at = '2020-01-01 00:00:05'
                 WHERE id = ?1",
                [&attempt.id],
            )?;
            Ok::<_, Error>(())
        })
        .unwrap();

        let waiting = manager
            .transition_attempt(
                &attempt.id,
                "waiting_for_input",
                "Waiting for input",
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            waiting.response_started_at.as_deref(),
            Some("2020-01-01 00:00:05")
        );
        let resumed = manager
            .transition_attempt(&attempt.id, "running", "Responding again", None, None)
            .unwrap();

        assert_eq!(resumed.started_at.as_deref(), Some("2020-01-01 00:00:00"));
        assert_ne!(
            resumed.response_started_at.as_deref(),
            Some("2020-01-01 00:00:05")
        );
    }

    #[test]
    fn event_provenance_is_preserved() {
        let db = Arc::new(Database::open_memory().unwrap());
        insert_agent(&db, "seo");
        let manager = SessionManager::new(db);
        manager.ensure("seo", None).unwrap();
        let event = manager
            .append_event(
                "seo",
                NewEvent {
                    attempt_id: None,
                    task_id: None,
                    source_type: "schedule",
                    source_id: Some("weekly-seo"),
                    event_type: "message_received",
                    summary: "Run the weekly SEO review",
                    payload: json!({ "cron": "0 9 * * 1" }),
                },
            )
            .unwrap();

        assert_eq!(event.source_type, "schedule");
        assert_eq!(event.source_id.as_deref(), Some("weekly-seo"));
        assert_eq!(event.payload["cron"], "0 9 * * 1");
    }

    #[test]
    fn task_activity_is_scoped_and_incremental() {
        let db = Arc::new(Database::open_memory().unwrap());
        insert_agent(&db, "builder");
        let manager = SessionManager::new(db.clone());
        manager.ensure("builder", Some("website")).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tasks (id, title) VALUES ('task-1', 'First'), ('task-2', 'Second')",
                [],
            )?;
            conn.execute(
                "INSERT INTO task_queue (id, task_id, agent_id, status) VALUES
                    (1, 'task-1', 'builder', 'queued'),
                    (2, 'task-2', 'builder', 'queued')",
                [],
            )?;
            Ok::<_, Error>(())
        })
        .unwrap();
        let first = manager
            .create_attempt(
                "builder", "task-1", 1, "codex", "task", "task", None, "First",
            )
            .unwrap();
        manager
            .create_attempt(
                "builder", "task-2", 2, "codex", "task", "task", None, "Second",
            )
            .unwrap();
        manager
            .transition_attempt(&first.id, "running", "Inspecting files", None, None)
            .unwrap();

        let initial = manager
            .task_activity("task-1", None, None, 100, 20)
            .unwrap();
        assert_eq!(initial.attempts.len(), 1);
        assert!(initial
            .events
            .iter()
            .all(|event| event.task_id.as_deref() == Some("task-1")));
        let last_id = initial.events.last().unwrap().id;

        manager
            .append_event(
                "builder",
                NewEvent {
                    attempt_id: Some(&first.id),
                    task_id: Some("task-1"),
                    source_type: "runner",
                    source_id: Some("codex"),
                    event_type: "runner_progress",
                    summary: "Ran tests",
                    payload: json!({}),
                },
            )
            .unwrap();
        let incremental = manager
            .task_activity("task-1", Some(last_id), None, 100, 20)
            .unwrap();
        assert_eq!(incremental.events.len(), 1);
        assert_eq!(incremental.events[0].summary, "Ran tests");
    }

    #[test]
    fn task_activity_pages_back_through_older_events() {
        let db = Arc::new(Database::open_memory().unwrap());
        insert_agent(&db, "builder");
        let manager = SessionManager::new(db.clone());
        manager.ensure("builder", Some("website")).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tasks (id, title) VALUES ('task-1', 'First')",
                [],
            )?;
            Ok::<_, Error>(())
        })
        .unwrap();

        for index in 0..7 {
            manager
                .append_event(
                    "builder",
                    NewEvent {
                        attempt_id: None,
                        task_id: Some("task-1"),
                        source_type: "runner",
                        source_id: Some("codex"),
                        event_type: "runner_progress",
                        summary: &format!("Event {index}"),
                        payload: json!({}),
                    },
                )
                .unwrap();
        }

        let latest = manager.task_activity("task-1", None, None, 3, 20).unwrap();
        assert_eq!(latest.events.len(), 3);
        assert!(latest.has_more_before);
        assert_eq!(latest.events[0].summary, "Event 4");

        let earlier = manager
            .task_activity("task-1", None, Some(latest.events[0].id), 3, 20)
            .unwrap();
        assert_eq!(earlier.events.len(), 3);
        assert!(earlier.has_more_before);
        assert_eq!(earlier.events[0].summary, "Event 1");
        assert_eq!(earlier.events[2].summary, "Event 3");

        let oldest = manager
            .task_activity("task-1", None, Some(earlier.events[0].id), 3, 20)
            .unwrap();
        assert_eq!(oldest.events.len(), 1);
        assert!(!oldest.has_more_before);
        assert_eq!(oldest.events[0].summary, "Event 0");
    }

    #[test]
    fn deleting_a_session_clears_task_and_queue_pointers() {
        let db = Arc::new(Database::open_memory().unwrap());
        insert_agent(&db, "builder");
        let manager = SessionManager::new(db.clone());
        manager.ensure("builder", Some("Builder")).unwrap();
        db.with_conn(|conn| {
            conn.execute("INSERT INTO tasks (id, title) VALUES ('task-1', 'Build it')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO task_queue (id, task_id, agent_id, status) VALUES (1, 'task-1', 'builder', 'queued')",
                [],
            )
            .unwrap();
        });
        manager
            .create_attempt(
                "builder",
                "task-1",
                1,
                "codex",
                "task",
                "task",
                Some("task-1"),
                "Build it",
            )
            .unwrap();

        manager.delete("builder").unwrap();
        assert!(manager.get("builder").is_err());
        db.with_conn(|conn| {
            let pointers: (Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT session_id, active_attempt_id FROM tasks WHERE id = 'task-1'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(pointers, (None, None));
            let queue_attempt: Option<String> = conn
                .query_row(
                    "SELECT attempt_id FROM task_queue WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(queue_attempt, None);
        });
    }

    #[test]
    fn terminal_attempt_cannot_be_revived_by_a_late_worker_transition() {
        let db = Arc::new(Database::open_memory().unwrap());
        insert_agent(&db, "builder");
        let manager = SessionManager::new(db.clone());
        manager.ensure("builder", Some("Builder")).unwrap();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO tasks (id, title) VALUES ('task-1', 'Build it')",
                [],
            )?;
            conn.execute(
                "INSERT INTO task_queue (id, task_id, agent_id, status)
                 VALUES (1, 'task-1', 'builder', 'running')",
                [],
            )?;
            Ok::<_, Error>(())
        })
        .unwrap();
        let attempt = manager
            .create_attempt(
                "builder", "task-1", 1, "codex", "task", "task", None, "Build it",
            )
            .unwrap();

        manager
            .transition_attempt(&attempt.id, "interrupted", "Stopped", None, None)
            .unwrap();
        let late_transition = manager
            .transition_attempt(&attempt.id, "running", "Started late", None, None)
            .unwrap();

        assert_eq!(late_transition.status, "interrupted");
        let events = manager.list_events("builder", None, 50).unwrap();
        assert!(!events
            .iter()
            .any(|event| event.event_type == "attempt_running"));
    }
}
