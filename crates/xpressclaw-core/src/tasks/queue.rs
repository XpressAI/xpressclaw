use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::db::Database;
use crate::error::{Error, Result};

/// A queued task item for harness dispatch.
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

        debug!(task_id, agent_id, queue_id = id, "enqueued task");
        self.get(id)
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::board::{CreateTask, TaskBoard};

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
}
