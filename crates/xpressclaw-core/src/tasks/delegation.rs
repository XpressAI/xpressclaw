//! Read-only progress for durable child Tasks, separate from native plan rows.

use serde::Serialize;

use crate::{db::Database, error::Result};

#[derive(Debug, Serialize)]
pub struct DelegatedWork {
    pub task_id: String,
    pub title: String,
    pub agent_id: Option<String>,
    pub status: String,
    pub activity_status: String,
    pub blocks_parent: bool,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub latest_event_type: Option<String>,
    pub latest_event_at: Option<String>,
}

/// Read one snapshot; indexed task/attempt/event lookups avoid transferring
/// child transcripts or making one HTTP request per child on every poll.
pub fn list(db: &Database, parent_task_id: &str) -> Result<Vec<DelegatedWork>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.agent_id, t.status, t.blocks_parent,
                t.created_at, t.updated_at, t.completed_at, t.backlog, t.start_after,
                a.status, a.started_at, a.completed_at, e.event_type, e.created_at,
                EXISTS(SELECT 1 FROM task_pull_requests pr WHERE pr.task_id=t.id
                       AND pr.status IN ('waiting', 'attention')) AS review_pending,
                EXISTS(SELECT 1 FROM tasks child WHERE child.parent_task_id=t.id
                       AND child.blocks_parent=1 AND child.status!='completed') AS child_pending,
                EXISTS(SELECT 1 FROM task_dependencies d JOIN tasks dependency ON dependency.id=d.depends_on_id
                       WHERE d.task_id=t.id AND dependency.status!='completed') AS dependency_pending
         FROM tasks t
         LEFT JOIN work_attempts a ON a.id=(
             SELECT id FROM work_attempts WHERE task_id=t.id
             ORDER BY CASE WHEN status IN ('preparing','running','waiting_for_input','review') THEN 0
                           WHEN status='queued' THEN 1 ELSE 2 END,
                      created_at DESC, rowid DESC LIMIT 1)
         LEFT JOIN session_events e ON e.id=(
             SELECT id FROM session_events WHERE task_id=t.id ORDER BY id DESC LIMIT 1)
         WHERE t.parent_task_id=?1 AND t.provenance!='native_plan'
         ORDER BY (t.status='completed'), t.blocks_parent DESC, t.created_at, t.id",
    )?;
    let rows = stmt.query_map([parent_task_id], |row| {
        let status: String = row.get(3)?;
        let attempt: Option<String> = row.get(10)?;
        let backlog: bool = row.get(8)?;
        let start_after: Option<String> = row.get(9)?;
        let review_pending: bool = row.get(15)?;
        let child_pending: bool = row.get(16)?;
        let dependency_pending: bool = row.get(17)?;
        let activity = if matches!(
            status.as_str(),
            "completed" | "cancelled" | "blocked" | "waiting_for_input"
        ) {
            status.as_str()
        } else if matches!(
            attempt.as_deref(),
            Some("preparing" | "running" | "waiting_for_input")
        ) {
            attempt.as_deref().unwrap()
        } else if review_pending || attempt.as_deref() == Some("review") {
            "awaiting_review"
        } else if backlog {
            "backlog"
        } else if super::planning::is_scheduled(start_after.as_deref()) {
            "scheduled"
        } else if child_pending {
            "waiting_for_subtasks"
        } else if dependency_pending {
            "waiting_for_dependencies"
        } else if attempt.as_deref() == Some("queued") {
            "queued"
        } else if matches!(
            attempt.as_deref(),
            Some("failed" | "interrupted" | "cancelled")
        ) {
            attempt.as_deref().unwrap()
        } else {
            "idle"
        };
        // Match the scheduler exactly: even a cancelled required child remains
        // a blocker until the user resolves it. Never imply that it succeeded.
        Ok(DelegatedWork {
            task_id: row.get(0)?,
            title: row.get(1)?,
            agent_id: row.get(2)?,
            blocks_parent: row.get::<_, bool>(4)? && status != "completed",
            activity_status: activity.to_string(),
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            started_at: row.get(11)?,
            finished_at: if matches!(status.as_str(), "completed" | "cancelled") {
                row.get::<_, Option<String>>(7)?.or(Some(row.get(6)?))
            } else {
                row.get(12)?
            },
            status,
            latest_event_type: row.get(13)?,
            latest_event_at: row.get(14)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Database {
        let db = Database::open_memory().unwrap();
        db.conn().execute_batch(
            "INSERT INTO tasks (id,title,status) VALUES ('parent','Parent','in_progress'), ('other','Other','pending');
             INSERT INTO tasks (id,title,parent_task_id,status,agent_id) VALUES
                ('child','Independent review','parent','in_progress','reviewer'),
                ('unrelated','Unrelated','other','in_progress','reviewer');
             INSERT INTO tasks (id,title,parent_task_id,status,provenance,blocks_parent) VALUES
                ('plan','Checklist step','parent','in_progress','native_plan',0);
             INSERT INTO logical_sessions (id,agent_id) VALUES ('session','reviewer');
             INSERT INTO work_attempts (id,session_id,task_id,runner,status,started_at,created_at) VALUES
                ('running','session','child','codex','running','2026-10-11 10:26:51','2026-10-11 10:26:00'),
                ('queued','session','child','codex','queued',NULL,'2026-10-11 10:31:44');
             INSERT INTO session_events (session_id,attempt_id,task_id,source_type,event_type,summary,created_at) VALUES
                ('session','running','child','native','tool_call','A very large command which must not enter the summary','2026-10-11 10:30:00'),
                ('session',NULL,'unrelated','native','agent_thought','Private other task','2026-10-11 10:31:00');"
        ).unwrap();
        db
    }

    #[test]
    fn delegated_work_is_scoped_and_prefers_executing_attempt_over_queued_guidance() {
        let db = fixture();
        let items = list(&db, "parent").unwrap();
        assert_eq!(items.len(), 1);
        let child = &items[0];
        assert_eq!(child.task_id, "child");
        assert_eq!(child.agent_id.as_deref(), Some("reviewer"));
        assert_eq!(child.activity_status, "running");
        assert_eq!(child.started_at.as_deref(), Some("2026-10-11 10:26:51"));
        assert!(child.blocks_parent);
        assert_eq!(child.latest_event_type.as_deref(), Some("tool_call"));
        assert_eq!(
            child.latest_event_at.as_deref(),
            Some("2026-10-11 10:30:00")
        );
        let json = serde_json::to_string(&items).unwrap();
        assert!(!json.contains("command which"));
        assert!(!json.contains("Private other task"));
        // Reading progress has no scheduling, completion, or transcript effects.
        assert_eq!(
            db.conn()
                .query_row(
                    "SELECT status FROM work_attempts WHERE id='queued'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "queued"
        );
        assert_eq!(
            db.conn()
                .query_row("SELECT count(*) FROM session_events", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn delegated_work_completion_and_cancellation_match_parent_gate() {
        let db = fixture();
        db.conn().execute_batch("UPDATE work_attempts SET status='completed',completed_at='2026-10-11 10:40:39';
            UPDATE tasks SET status='completed',completed_at='2026-10-11 10:40:40' WHERE id='child';").unwrap();
        let completed = list(&db, "parent").unwrap();
        assert_eq!(completed[0].activity_status, "completed");
        assert!(!completed[0].blocks_parent);
        assert_eq!(
            completed[0].finished_at.as_deref(),
            Some("2026-10-11 10:40:40")
        );
        db.conn().execute("UPDATE tasks SET status='cancelled',completed_at=NULL,updated_at='2026-10-11 11:00:00' WHERE id='child'", []).unwrap();
        let cancelled = list(&db, "parent").unwrap();
        assert_eq!(cancelled[0].activity_status, "cancelled");
        assert!(cancelled[0].blocks_parent);
        assert_eq!(
            cancelled[0].finished_at.as_deref(),
            Some("2026-10-11 11:00:00")
        );
        db.conn()
            .execute("UPDATE tasks SET blocks_parent=0 WHERE id='child'", [])
            .unwrap();
        assert!(!list(&db, "parent").unwrap()[0].blocks_parent);
    }

    #[test]
    fn delegated_work_distinguishes_idle_failure_input_and_descendant_waits() {
        let db = fixture();
        db.conn().execute_batch("UPDATE work_attempts SET status='completed';
            INSERT INTO tasks(id,title,parent_task_id,status) VALUES('grandchild','Review tests','child','pending');").unwrap();
        assert_eq!(
            list(&db, "parent").unwrap()[0].activity_status,
            "waiting_for_subtasks"
        );
        db.conn()
            .execute(
                "UPDATE tasks SET status='completed' WHERE id='grandchild'",
                [],
            )
            .unwrap();
        assert_eq!(list(&db, "parent").unwrap()[0].activity_status, "idle");
        for status in [
            "failed",
            "interrupted",
            "queued",
            "preparing",
            "waiting_for_input",
        ] {
            db.conn()
                .execute(
                    "UPDATE work_attempts SET status=?1 WHERE id='queued'",
                    [status],
                )
                .unwrap();
            assert_eq!(list(&db, "parent").unwrap()[0].activity_status, status);
        }
    }

    #[test]
    fn delegated_work_distinguishes_unscheduled_work_and_dependencies() {
        let db = fixture();
        db.conn()
            .execute_batch(
                "UPDATE work_attempts SET status='queued';
            UPDATE tasks SET backlog=1 WHERE id='child';",
            )
            .unwrap();
        assert_eq!(list(&db, "parent").unwrap()[0].activity_status, "backlog");
        db.conn()
            .execute(
                "UPDATE tasks SET backlog=0,start_after='2999-01-01T00:00:00Z' WHERE id='child'",
                [],
            )
            .unwrap();
        assert_eq!(list(&db, "parent").unwrap()[0].activity_status, "scheduled");
        db.conn()
            .execute_batch(
                "UPDATE tasks SET start_after=NULL WHERE id='child';
            INSERT INTO task_dependencies(task_id,depends_on_id) VALUES('child','other');",
            )
            .unwrap();
        assert_eq!(
            list(&db, "parent").unwrap()[0].activity_status,
            "waiting_for_dependencies"
        );
        db.conn()
            .execute("UPDATE tasks SET status='completed' WHERE id='other'", [])
            .unwrap();
        assert_eq!(list(&db, "parent").unwrap()[0].activity_status, "queued");
    }
}
