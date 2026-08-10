use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tracing::debug;
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};
use crate::sessions::SessionManager;

/// A queued task item for native attempt dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueItem {
    pub id: i64,
    pub task_id: String,
    pub agent_id: String,
    pub status: String,
    pub queued_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub harness_response: Option<String>,
    pub attempt_id: Option<String>,
}

/// SQLite-backed task queue for dispatching work to harness containers.
pub struct TaskQueue {
    db: Arc<Database>,
}

impl TaskQueue {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Enqueue a task for an agent.
    pub fn enqueue(&self, task_id: &str, agent_id: &str) -> Result<QueueItem> {
        let id = self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO task_queue (task_id, agent_id, status) VALUES (?1, ?2, 'queued')",
                rusqlite::params![task_id, agent_id],
            )?;
            Ok::<_, Error>(conn.last_insert_rowid())
        })?;

        self.create_attempt_for_item(id, task_id, agent_id)
    }

    /// Ensure a task has exactly one dispatchable queue item. Workflow
    /// recovery uses this after persisting task ownership but before (or after
    /// an interrupted) initial dispatch.
    pub fn ensure_enqueued(&self, task_id: &str, agent_id: &str) -> Result<Option<QueueItem>> {
        let id = self.db.with_conn(|conn| {
            let active = conn
                .query_row(
                    "SELECT id, attempt_id FROM task_queue
                     WHERE task_id = ?1 AND status IN ('queued', 'running')
                     ORDER BY id DESC LIMIT 1",
                    [task_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()?;
            if let Some((id, attempt_id)) = active {
                return Ok::<_, Error>(attempt_id.is_none().then_some(id));
            }
            conn.execute(
                "INSERT INTO task_queue (task_id, agent_id, status) VALUES (?1, ?2, 'queued')",
                rusqlite::params![task_id, agent_id],
            )?;
            Ok(Some(conn.last_insert_rowid()))
        })?;

        id.map(|id| self.create_attempt_for_item(id, task_id, agent_id))
            .transpose()
    }

    /// Enqueue one continuation turn unless the task already has a queued
    /// turn. A running turn is intentionally not considered a duplicate: the
    /// continuation waits behind it and receives any messages sent meanwhile.
    pub fn enqueue_continuation(&self, task_id: &str, agent_id: &str) -> Result<Option<QueueItem>> {
        let id = self.db.with_conn(|conn| {
            let changed = conn.execute(
                "INSERT INTO task_queue (task_id, agent_id, status)
                 SELECT ?1, ?2, 'queued'
                 WHERE NOT EXISTS (
                    SELECT 1 FROM task_queue WHERE task_id = ?1 AND status = 'queued'
                 )",
                rusqlite::params![task_id, agent_id],
            )?;
            Ok::<_, Error>((changed == 1).then(|| conn.last_insert_rowid()))
        })?;

        id.map(|id| self.create_attempt_for_item(id, task_id, agent_id))
            .transpose()
    }

    /// Add GitHub review feedback and enqueue a continuation for the task's
    /// current assignment as one atomic unit.
    ///
    /// GitHub review polling performs network I/O before it knows a follow-up
    /// is necessary. The task may be reassigned or cancelled during that
    /// await, so callers must not reuse the agent or status from their older
    /// polling snapshot here. A terminal task returns `None` without adding a
    /// message or creating work.
    pub(crate) fn enqueue_review_follow_up_for_current_agent(
        &self,
        task_id: &str,
        message: &str,
    ) -> Result<Option<(String, Option<QueueItem>)>> {
        let outcome = self.db.with_conn(|conn| {
            let transaction = conn.unchecked_transaction()?;
            let (agent_id, title, description, context, status): (
                Option<String>,
                String,
                Option<String>,
                Option<String>,
                String,
            ) = transaction.query_row(
                "SELECT agent_id, title, description, context, status
                 FROM tasks WHERE id = ?1",
                [task_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?;
            if matches!(status.as_str(), "completed" | "cancelled") {
                transaction.commit()?;
                return Ok::<_, Error>(None);
            }
            let agent_id = agent_id
                .filter(|agent_id| !agent_id.trim().is_empty())
                .ok_or_else(|| {
                    Error::Task("cannot queue review feedback for an unassigned task".into())
                })?;
            transaction.execute(
                "INSERT INTO task_messages (task_id, role, content) VALUES (?1, 'user', ?2)",
                rusqlite::params![task_id, message],
            )?;
            let already_queued: bool = transaction.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM task_queue WHERE task_id = ?1 AND status = 'queued'
                 )",
                [task_id],
                |row| row.get(0),
            )?;
            if already_queued {
                transaction.execute(
                    "UPDATE tasks SET status = 'in_progress', completed_at = NULL,
                        updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    [task_id],
                )?;
                transaction.commit()?;
                return Ok::<_, Error>(Some((agent_id, None)));
            }

            transaction.execute(
                "INSERT INTO task_queue (task_id, agent_id, status) VALUES (?1, ?2, 'queued')",
                rusqlite::params![task_id, agent_id],
            )?;
            let queue_id = transaction.last_insert_rowid();
            let context = context
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
            let kind = context
                .as_ref()
                .and_then(|value| value.get("kind"))
                .and_then(|value| value.as_str())
                .unwrap_or("task");
            let source_type = context
                .as_ref()
                .and_then(|value| value.get("origin"))
                .and_then(|value| value.as_str())
                .unwrap_or("task");
            let source_id = context
                .as_ref()
                .and_then(|value| value.get("source_id"))
                .and_then(|value| value.as_str())
                .unwrap_or(task_id);
            let prompt = match description {
                Some(description) if !description.trim().is_empty() => {
                    format!("{title}\n\n{description}")
                }
                _ => title,
            };
            let attempt_id = Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT OR IGNORE INTO logical_sessions (id, agent_id) VALUES (?1, ?1)",
                [&agent_id],
            )?;
            transaction.execute(
                "INSERT INTO work_attempts
                 (id, session_id, task_id, queue_id, kind, runner, status, prompt)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'auto', 'queued', ?6)",
                rusqlite::params![attempt_id, agent_id, task_id, queue_id, kind, prompt],
            )?;
            transaction.execute(
                "UPDATE task_queue SET attempt_id = ?1 WHERE id = ?2",
                rusqlite::params![attempt_id, queue_id],
            )?;
            transaction.execute(
                "UPDATE tasks SET session_id = ?1, active_attempt_id = ?2,
                    status = 'in_progress', completed_at = NULL,
                    updated_at = CURRENT_TIMESTAMP WHERE id = ?3",
                rusqlite::params![agent_id, attempt_id, task_id],
            )?;
            transaction.execute(
                "UPDATE logical_sessions SET status = 'queued', updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1 AND status != 'running'",
                [&agent_id],
            )?;
            transaction.execute(
                "INSERT INTO session_events
                 (session_id, attempt_id, task_id, source_type, source_id,
                  event_type, summary, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'attempt_queued', 'Work queued', ?6)",
                rusqlite::params![
                    agent_id,
                    attempt_id,
                    task_id,
                    source_type,
                    source_id,
                    serde_json::json!({ "runner": "auto", "kind": kind, "queue_id": queue_id })
                        .to_string(),
                ],
            )?;
            transaction.execute(
                "UPDATE logical_sessions
                 SET latest_summary = 'Work queued', updated_at = CURRENT_TIMESTAMP
                 WHERE id = ?1",
                [&agent_id],
            )?;
            transaction.commit()?;
            Ok(Some((agent_id, Some(queue_id))))
        })?;

        let Some((agent_id, queue_id)) = outcome else {
            return Ok(None);
        };
        let item = queue_id.map(|queue_id| self.get(queue_id)).transpose()?;
        Ok(Some((agent_id, item)))
    }

    fn create_attempt_for_item(&self, id: i64, task_id: &str, agent_id: &str) -> Result<QueueItem> {
        let (title, description, kind, source_type, source_id) = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT title, description, context FROM tasks WHERE id = ?1",
                [task_id],
                |row| {
                    let title: String = row.get(0)?;
                    let description: Option<String> = row.get(1)?;
                    let context: Option<String> = row.get(2)?;
                    let context = context
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());
                    let kind = context
                        .as_ref()
                        .and_then(|value| value.get("kind"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("task")
                        .to_string();
                    let source_type = context
                        .as_ref()
                        .and_then(|value| value.get("origin"))
                        .and_then(|value| value.as_str())
                        .unwrap_or("task")
                        .to_string();
                    let source_id = context
                        .as_ref()
                        .and_then(|value| value.get("source_id"))
                        .and_then(|value| value.as_str())
                        .map(str::to_owned);
                    Ok((title, description, kind, source_type, source_id))
                },
            )
            .map_err(Error::from)
        })?;
        let prompt = match description {
            Some(description) if !description.trim().is_empty() => {
                format!("{title}\n\n{description}")
            }
            _ => title,
        };
        if let Err(error) = SessionManager::new(self.db.clone()).create_attempt(
            agent_id,
            task_id,
            id,
            "auto",
            &kind,
            &source_type,
            source_id.as_deref(),
            &prompt,
        ) {
            self.db
                .with_conn(|conn| conn.execute("DELETE FROM task_queue WHERE id = ?1", [id]))?;
            return Err(error);
        }

        debug!(task_id, agent_id, queue_id = id, "enqueued task");
        self.get(id)
    }

    /// Whether a continuation is waiting behind the current turn.
    pub fn has_queued_for_task(&self, task_id: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM task_queue WHERE task_id = ?1 AND status = 'queued'
                )",
                [task_id],
                |row| row.get(0),
            )
            .map_err(Error::from)
        })
    }

    /// Get a queue item by ID.
    pub fn get(&self, id: i64) -> Result<QueueItem> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT * FROM task_queue WHERE id = ?1")?;
            stmt.query_row([id], |row| Ok(row_to_item(row)))
                .map_err(|_| Error::Task(format!("queue item {id} not found")))
        })?
    }

    /// Claim the next queued item for an agent (atomically set to 'running').
    pub fn claim(&self, agent_id: &str) -> Result<Option<QueueItem>> {
        self.db.with_conn(|conn| {
            // Find the next queued item for this agent
            let mut stmt = conn.prepare(
                "SELECT id FROM task_queue WHERE agent_id = ?1 AND status = 'queued' ORDER BY queued_at ASC LIMIT 1",
            )?;

            let id: Option<i64> = stmt
                .query_row([agent_id], |row| row.get(0))
                .ok();

            match id {
                Some(id) => {
                    conn.execute(
                        "UPDATE task_queue SET status = 'running', started_at = CURRENT_TIMESTAMP WHERE id = ?1",
                        [id],
                    )?;
                    // Need to drop conn before calling self.get
                    Ok(Some(id))
                }
                None => Ok(None),
            }
        }).and_then(|opt_id| {
            match opt_id {
                Some(id) => self.get(id).map(Some),
                None => Ok(None),
            }
        })
    }

    /// Claim the oldest queued item across all logical sessions.
    ///
    /// Dispatch is serialized per project even though several retained ACP
    /// project processes may run concurrently.
    pub fn claim_next(&self) -> Result<Option<QueueItem>> {
        self.db
            .with_conn(|conn| {
                let id: Option<i64> = conn
                    .query_row(
                        "SELECT q.id FROM task_queue q
                         JOIN tasks t ON t.id = q.task_id
                         JOIN work_attempts candidate ON candidate.id = q.attempt_id
                         WHERE q.status = 'queued'
                           AND candidate.status = 'queued'
                           AND NOT EXISTS (
                               SELECT 1 FROM task_dependencies d
                               JOIN tasks dependency ON dependency.id = d.depends_on_id
                               WHERE d.task_id = q.task_id AND dependency.status != 'completed'
                           )
                           AND NOT EXISTS (
                               SELECT 1 FROM work_attempts active
                               WHERE active.session_id = candidate.session_id
                                 AND active.id != candidate.id
                                 AND active.status IN (
                                     'preparing', 'running', 'waiting_for_input', 'review'
                                 )
                           )
                           AND NOT EXISTS (
                               SELECT 1 FROM task_queue active_dispatch
                               JOIN work_attempts active_owner
                                 ON active_owner.id = active_dispatch.attempt_id
                               WHERE active_dispatch.agent_id = q.agent_id
                                 AND active_dispatch.status = 'running'
                                 AND (
                                     active_owner.status NOT IN (
                                         'completed', 'failed', 'cancelled', 'interrupted'
                                     )
                                     OR active_owner.container_id IS NOT NULL
                                 )
                           )
                           AND NOT EXISTS (
                               SELECT 1 FROM task_pull_requests monitored_pr
                               JOIN tasks monitored_task
                                 ON monitored_task.id = monitored_pr.task_id
                               WHERE monitored_pr.agent_id = q.agent_id
                                 AND monitored_pr.task_id != q.task_id
                                 AND monitored_pr.status IN ('waiting', 'attention')
                                 AND monitored_task.status NOT IN ('completed', 'cancelled')
                           )
                         ORDER BY t.priority DESC, q.queued_at ASC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .ok();
                if let Some(id) = id {
                    let changed = conn.execute(
                        "UPDATE task_queue SET status = 'running', started_at = CURRENT_TIMESTAMP
                         WHERE id = ?1 AND status = 'queued'",
                        [id],
                    )?;
                    if changed == 1 {
                        return Ok(Some(id));
                    }
                }
                Ok(None)
            })
            .and_then(|id| id.map(|id| self.get(id)).transpose())
    }

    /// Requeue work that was in flight when the control plane stopped. Any
    /// corresponding project containers are stopped by the server before
    /// dispatch begins, so the same logical attempt can be safely resumed from
    /// queued state without leaving a task permanently stuck as in-progress.
    pub fn recover_in_progress(&self) -> Result<usize> {
        let finalized = self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            let finalized = tx.execute(
                "UPDATE task_queue
                 SET status = CASE
                     WHEN (SELECT status FROM work_attempts WHERE id = task_queue.attempt_id) = 'completed'
                         THEN 'completed'
                     ELSE 'failed'
                 END,
                 completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP),
                 harness_response = COALESCE(
                     harness_response,
                     (SELECT COALESCE(result, error_message, status)
                      FROM work_attempts WHERE id = task_queue.attempt_id)
                 )
                 WHERE status = 'running'
                   AND EXISTS (
                       SELECT 1 FROM work_attempts terminal
                       WHERE terminal.id = task_queue.attempt_id
                         AND terminal.status IN ('completed', 'failed', 'cancelled', 'interrupted')
                   )",
                [],
            )?;
            tx.execute(
                "UPDATE work_attempts SET container_id = NULL
                 WHERE container_id IS NOT NULL
                   AND status IN ('completed', 'failed', 'cancelled', 'interrupted')",
                [],
            )?;
            tx.commit()?;
            Ok::<_, Error>(finalized)
        })?;
        if finalized > 0 {
            debug!(finalized, "recovered terminal queue dispatches");
        }

        let orphaned: Vec<(i64, String, String, String)> = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT q.id, a.id, a.session_id, q.task_id
                 FROM task_queue q
                 JOIN work_attempts a ON a.id = q.attempt_id
                 WHERE q.status = 'running'
                   AND a.status IN (
                       'queued', 'preparing', 'running', 'waiting_for_input', 'review'
                   )",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .filter_map(|row| row.ok())
                .collect();
            Ok::<_, Error>(rows)
        })?;

        if orphaned.is_empty() {
            return Ok(0);
        }

        self.db.with_conn(|conn| {
            let tx = conn.unchecked_transaction()?;
            for (queue_id, attempt_id, session_id, task_id) in &orphaned {
                tx.execute(
                    "UPDATE task_queue SET status = 'queued', started_at = NULL,
                        completed_at = NULL WHERE id = ?1",
                    [queue_id],
                )?;
                tx.execute(
                    "UPDATE work_attempts SET status = 'queued', container_id = NULL,
                        started_at = NULL, completed_at = NULL, error_message = NULL
                     WHERE id = ?1",
                    [attempt_id],
                )?;
                tx.execute(
                    "UPDATE tasks SET status = 'pending', completed_at = NULL,
                        updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    [task_id],
                )?;
                tx.execute(
                    "UPDATE logical_sessions SET status = 'queued',
                        updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
                    [session_id],
                )?;
            }
            tx.commit()?;
            Ok::<_, Error>(())
        })?;

        let sessions = SessionManager::new(self.db.clone());
        for (_, attempt_id, session_id, task_id) in &orphaned {
            let _ = sessions.append_event(
                session_id,
                crate::sessions::NewEvent {
                    attempt_id: Some(attempt_id),
                    task_id: Some(task_id),
                    source_type: "system",
                    source_id: Some("startup-recovery"),
                    event_type: "attempt_requeued",
                    summary: "Interrupted work requeued after restart",
                    payload: serde_json::json!({}),
                },
            );
        }

        Ok(orphaned.len())
    }

    /// Mark a queue item as completed with the harness response.
    pub fn complete(&self, id: i64, response: &str) -> Result<QueueItem> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_queue SET status = 'completed', completed_at = CURRENT_TIMESTAMP, harness_response = ?1 WHERE id = ?2",
                rusqlite::params![response, id],
            )
        })?;

        self.get(id)
    }

    /// Mark a queue item as failed.
    pub fn fail(&self, id: i64, error: &str) -> Result<QueueItem> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_queue SET status = 'failed', completed_at = CURRENT_TIMESTAMP, harness_response = ?1 WHERE id = ?2",
                rusqlite::params![error, id],
            )
        })?;

        self.get(id)
    }

    /// List queue items, optionally filtered by agent and/or status.
    pub fn list(
        &self,
        agent_id: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<QueueItem>> {
        self.db.with_conn(|conn| {
            let mut sql = "SELECT * FROM task_queue WHERE 1=1".to_string();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

            if let Some(aid) = agent_id {
                sql.push_str(" AND agent_id = ?");
                params.push(Box::new(aid.to_string()));
            }
            if let Some(s) = status {
                sql.push_str(" AND status = ?");
                params.push(Box::new(s.to_string()));
            }
            sql.push_str(" ORDER BY queued_at DESC LIMIT ?");
            params.push(Box::new(limit));

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut stmt = conn.prepare(&sql)?;
            let items = stmt
                .query_map(param_refs.as_slice(), |row| Ok(row_to_item(row)))
                .map_err(|e| Error::Database(e.to_string()))?
                .filter_map(|r| r.ok())
                .filter_map(|r| r.ok())
                .collect();

            Ok(items)
        })
    }

    /// Count of queued items for an agent.
    pub fn pending_count(&self, agent_id: &str) -> Result<i64> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT COUNT(*) FROM task_queue WHERE agent_id = ?1 AND status = 'queued'",
            )?;
            stmt.query_row([agent_id], |row| row.get(0))
                .map_err(|e| Error::Database(e.to_string()))
        })
    }
}

fn row_to_item(row: &rusqlite::Row) -> Result<QueueItem> {
    Ok(QueueItem {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        agent_id: row.get("agent_id")?,
        status: row.get("status")?,
        queued_at: row.get("queued_at")?,
        started_at: row.get("started_at")?,
        completed_at: row.get("completed_at")?,
        harness_response: row.get("harness_response")?,
        attempt_id: row.get("attempt_id").unwrap_or(None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::board::{CreateTask, TaskBoard, TaskStatus};

    fn setup() -> (Arc<Database>, TaskQueue) {
        let db = Arc::new(Database::open_memory().unwrap());
        let queue = TaskQueue::new(db.clone());
        (db, queue)
    }

    #[test]
    fn test_enqueue_and_claim() {
        let (db, queue) = setup();

        // Create a task first (for foreign key)
        let board = TaskBoard::new(db);
        let task = board
            .create(&CreateTask {
                title: "Test task".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        // Enqueue
        let item = queue.enqueue(&task.id, "atlas").unwrap();
        assert_eq!(item.status, "queued");
        assert_eq!(item.agent_id, "atlas");

        // Claim
        let claimed = queue.claim("atlas").unwrap().unwrap();
        assert_eq!(claimed.id, item.id);
        assert_eq!(claimed.status, "running");
        assert!(claimed.started_at.is_some());

        // No more items to claim
        assert!(queue.claim("atlas").unwrap().is_none());
    }

    #[test]
    fn ensure_enqueued_does_not_duplicate_active_dispatch() {
        let (db, queue) = setup();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Owned workflow task".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        let first = queue.ensure_enqueued(&task.id, "atlas").unwrap().unwrap();
        assert!(queue.ensure_enqueued(&task.id, "atlas").unwrap().is_none());
        assert_eq!(queue.claim("atlas").unwrap().unwrap().id, first.id);
        assert!(queue.ensure_enqueued(&task.id, "atlas").unwrap().is_none());
        let dispatches: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM task_queue WHERE task_id = ?1",
                    [&task.id],
                    |row| row.get(0),
                )
            })
            .unwrap();
        assert_eq!(dispatches, 1);
    }

    #[test]
    fn recovers_interrupted_native_attempts() {
        let (db, queue) = setup();
        let board = TaskBoard::new(db.clone());
        let task = board
            .create(&CreateTask {
                title: "Interrupted task".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let queued = queue.enqueue(&task.id, "atlas").unwrap();
        let claimed = queue.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, queued.id);
        let attempt_id = claimed.attempt_id.as_deref().unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(attempt_id, "running", "Working", None, None)
            .unwrap();
        board
            .update_status(&task.id, "in_progress", Some("atlas"))
            .unwrap();

        assert_eq!(queue.recover_in_progress().unwrap(), 1);
        assert_eq!(queue.get(queued.id).unwrap().status, "queued");
        assert_eq!(board.get(&task.id).unwrap().status.as_str(), "pending");
        let overview = SessionManager::new(db).overview("atlas").unwrap();
        assert_eq!(overview.session.status, "queued");
        assert!(overview
            .recent_events
            .iter()
            .any(|event| event.event_type == "attempt_requeued"));
    }

    #[test]
    fn recovery_finalizes_terminal_dispatch_and_releases_container_lease() {
        let (db, queue) = setup();
        let task = TaskBoard::new(db.clone())
            .create(&CreateTask {
                title: "Finished before restart".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let queued = queue.enqueue(&task.id, "atlas").unwrap();
        let claimed = queue.claim_next().unwrap().unwrap();
        let attempt_id = claimed.attempt_id.as_deref().unwrap();
        let sessions = SessionManager::new(db);
        sessions
            .set_container(attempt_id, "retained-project-container")
            .unwrap();
        sessions
            .transition_attempt(attempt_id, "completed", "Done", Some("result"), None)
            .unwrap();

        assert_eq!(queue.recover_in_progress().unwrap(), 0);
        assert_eq!(queue.get(queued.id).unwrap().status, "completed");
        assert!(sessions
            .get_attempt(attempt_id)
            .unwrap()
            .container_id
            .is_none());
    }

    #[test]
    fn waiting_attempt_serializes_the_logical_session() {
        let (db, queue) = setup();
        let board = TaskBoard::new(db.clone());
        let first_task = board
            .create(&CreateTask {
                title: "Needs an answer".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let second_task = board
            .create(&CreateTask {
                title: "Queued behind the answer".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let first_item = queue.enqueue(&first_task.id, "atlas").unwrap();
        let second_item = queue.enqueue(&second_task.id, "atlas").unwrap();

        let claimed = queue.claim_next().unwrap().unwrap();
        let waiting_attempt_id = claimed.attempt_id.as_deref().unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                waiting_attempt_id,
                "waiting_for_input",
                "Waiting for an answer",
                None,
                None,
            )
            .unwrap();

        assert!(queue.claim_next().unwrap().is_none());
        let unclaimed_id = if claimed.id == first_item.id {
            second_item.id
        } else {
            first_item.id
        };
        assert_eq!(queue.get(unclaimed_id).unwrap().status, "queued");

        SessionManager::new(db)
            .transition_attempt(
                waiting_attempt_id,
                "completed",
                "Answer received",
                Some("done"),
                None,
            )
            .unwrap();
        assert_eq!(queue.claim_next().unwrap().unwrap().id, unclaimed_id);
    }

    #[test]
    fn running_dispatch_holds_project_lease_until_container_cleanup_finishes() {
        let (db, queue) = setup();
        let board = TaskBoard::new(db.clone());
        let first_task = board
            .create(&CreateTask {
                title: "Cancel this turn".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let second_task = board
            .create(&CreateTask {
                title: "Run after cleanup".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let first = queue.enqueue(&first_task.id, "atlas").unwrap();
        let claimed = queue.claim_next().unwrap().unwrap();
        assert_eq!(claimed.id, first.id);
        let second = queue.enqueue(&second_task.id, "atlas").unwrap();

        let sessions = SessionManager::new(db);
        sessions
            .set_container(
                claimed.attempt_id.as_deref().unwrap(),
                "retained-project-container",
            )
            .unwrap();
        sessions
            .transition_attempt(
                claimed.attempt_id.as_deref().unwrap(),
                "cancelled",
                "Cancellation requested",
                None,
                None,
            )
            .unwrap();

        // The terminal attempt no longer provides the ordinary session lock,
        // but its running dispatch keeps the shared project container leased
        // until the cancellation path has stopped it.
        assert!(queue.claim_next().unwrap().is_none());
        sessions
            .clear_container(claimed.attempt_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(queue.claim_next().unwrap().unwrap().id, second.id);
    }

    #[test]
    fn claimed_dispatch_serializes_project_before_container_startup() {
        let (db, queue) = setup();
        let board = TaskBoard::new(db);
        for title in ["First turn", "Second turn"] {
            let task = board
                .create(&CreateTask {
                    title: title.into(),
                    description: None,
                    agent_id: Some("atlas".into()),
                    parent_task_id: None,
                    sop_id: None,
                    conversation_id: None,
                    priority: None,
                    context: None,
                })
                .unwrap();
            queue.enqueue(&task.id, "atlas").unwrap();
        }

        // The first claim changes the dispatch row before its worker has time
        // to transition the attempt or attach the shared project container.
        assert!(queue.claim_next().unwrap().is_some());
        assert!(queue.claim_next().unwrap().is_none());
    }

    #[test]
    fn pending_pr_review_reserves_agent_lane_but_allows_same_task_follow_up() {
        use crate::workers::github_review::GithubReviewManager;

        let (db, queue) = setup();
        let board = TaskBoard::new(db.clone());
        let review_task = board
            .create(&CreateTask {
                title: "Await PR review".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let next_task = board
            .create(&CreateTask {
                title: "Must wait for review".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        GithubReviewManager::new(db.clone())
            .register(
                &review_task.id,
                "atlas",
                "XpressAI/example",
                "https://github.com/XpressAI/example/pull/7",
            )
            .unwrap();
        let waiting = queue.enqueue(&next_task.id, "atlas").unwrap();
        assert!(queue.claim_next().unwrap().is_none());

        let follow_up = queue
            .enqueue_continuation(&review_task.id, "atlas")
            .unwrap()
            .unwrap();
        assert_eq!(queue.claim_next().unwrap().unwrap().id, follow_up.id);
        assert_eq!(queue.get(waiting.id).unwrap().status, "queued");
    }

    #[test]
    fn terminal_pull_request_releases_lane_while_task_has_remaining_work() {
        use crate::workers::github_review::GithubReviewManager;

        let (db, queue) = setup();
        let board = TaskBoard::new(db.clone());
        let review_task = board
            .create(&CreateTask {
                title: "Await PR review".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let next_task = board
            .create(&CreateTask {
                title: "Run after review".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        GithubReviewManager::new(db.clone())
            .register(
                &review_task.id,
                "atlas",
                "XpressAI/example",
                "https://github.com/XpressAI/example/pull/7",
            )
            .unwrap();
        let next = queue.enqueue(&next_task.id, "atlas").unwrap();
        assert!(queue.claim_next().unwrap().is_none());
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE task_pull_requests SET status = 'approved' WHERE task_id = ?1",
                [&review_task.id],
            )?;
            Ok::<_, crate::error::Error>(())
        })
        .unwrap();
        assert_eq!(
            board.get(&review_task.id).unwrap().status,
            TaskStatus::InProgress
        );
        assert_eq!(queue.claim_next().unwrap().unwrap().id, next.id);
    }

    #[test]
    fn recovers_waiting_attempts_after_restart() {
        let (db, queue) = setup();
        let board = TaskBoard::new(db.clone());
        let task = board
            .create(&CreateTask {
                title: "Waiting when the server stopped".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let queued = queue.enqueue(&task.id, "atlas").unwrap();
        let claimed = queue.claim_next().unwrap().unwrap();
        let attempt_id = claimed.attempt_id.as_deref().unwrap();
        let sessions = SessionManager::new(db.clone());
        sessions
            .transition_attempt(
                attempt_id,
                "waiting_for_input",
                "Waiting for an answer",
                None,
                None,
            )
            .unwrap();
        sessions
            .set_container(attempt_id, "stopped-container")
            .unwrap();
        board
            .update_status(&task.id, "waiting_for_input", Some("atlas"))
            .unwrap();

        assert_eq!(queue.recover_in_progress().unwrap(), 1);
        assert_eq!(queue.get(queued.id).unwrap().status, "queued");
        assert_eq!(board.get(&task.id).unwrap().status.as_str(), "pending");
        let recovered = sessions.get_attempt(attempt_id).unwrap();
        assert_eq!(recovered.status, "queued");
        assert!(recovered.container_id.is_none());
        assert_eq!(sessions.overview("atlas").unwrap().session.status, "queued");
    }

    #[test]
    fn test_complete_and_fail() {
        let (db, queue) = setup();

        let board = TaskBoard::new(db);
        let task = board
            .create(&CreateTask {
                title: "Complete test".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        let item = queue.enqueue(&task.id, "atlas").unwrap();
        let completed = queue
            .complete(item.id, "Task completed successfully")
            .unwrap();
        assert_eq!(completed.status, "completed");
        assert!(completed.harness_response.is_some());
    }

    #[test]
    fn test_pending_count() {
        let (db, queue) = setup();

        let board = TaskBoard::new(db);
        let t1 = board
            .create(&CreateTask {
                title: "T1".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();
        let t2 = board
            .create(&CreateTask {
                title: "T2".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        queue.enqueue(&t1.id, "atlas").unwrap();
        queue.enqueue(&t2.id, "atlas").unwrap();

        assert_eq!(queue.pending_count("atlas").unwrap(), 2);

        queue.claim("atlas").unwrap();
        assert_eq!(queue.pending_count("atlas").unwrap(), 1);
    }

    #[test]
    fn coalesces_messages_into_one_queued_continuation() {
        let (db, queue) = setup();
        let board = TaskBoard::new(db);
        let task = board
            .create(&CreateTask {
                title: "Conversation".into(),
                description: None,
                agent_id: Some("atlas".into()),
                parent_task_id: None,
                sop_id: None,
                conversation_id: None,
                priority: None,
                context: None,
            })
            .unwrap();

        let first = queue.enqueue(&task.id, "atlas").unwrap();
        assert!(queue
            .enqueue_continuation(&task.id, "atlas")
            .unwrap()
            .is_none());
        let running = queue.claim("atlas").unwrap().unwrap();
        assert_eq!(running.id, first.id);

        let continuation = queue
            .enqueue_continuation(&task.id, "atlas")
            .unwrap()
            .unwrap();
        assert_ne!(continuation.id, first.id);
        assert!(queue.has_queued_for_task(&task.id).unwrap());
        assert!(queue
            .enqueue_continuation(&task.id, "atlas")
            .unwrap()
            .is_none());
        assert_eq!(queue.pending_count("atlas").unwrap(), 1);
    }
}
