//! Durable child failures wake their parent once, without opening its completion gate.

use rusqlite::{params, OptionalExtension, Transaction};

use super::{conversation::TaskConversation, queue::TaskQueue};
use crate::{
    db::Database,
    error::{Error, Result},
};

// This is also used at claim time: an old notification must not grant a recovery
// turn after the child has retried, completed, or moved to another parent.
const OUTCOMES: &str = "
 SELECT child.id AS child_id, child.parent_task_id AS parent_id, child.title,
        child.agent_id, COALESCE(a.id, '') AS attempt_id,
        CASE WHEN child.status='cancelled' THEN 'cancelled'
             WHEN child.status='blocked' THEN 'blocked'
             WHEN child.status='waiting_for_input' OR a.status='waiting_for_input'
             THEN 'waiting_for_input' END AS outcome,
        CASE WHEN a.status='failed' THEN a.error_message END AS error_message
 FROM tasks child
 LEFT JOIN work_attempts a ON a.id=(SELECT id FROM work_attempts
     WHERE task_id=child.id ORDER BY created_at DESC, rowid DESC LIMIT 1)
 WHERE child.parent_task_id IS NOT NULL AND child.hidden=0
   AND child.provenance!='native_plan' AND child.status!='completed'
   AND NOT EXISTS (SELECT 1 FROM work_attempts active WHERE active.task_id=child.id
       AND active.status IN ('queued','preparing','running','review'))";

pub(super) fn recovery_grant_sql() -> String {
    format!(
        "EXISTS (
        SELECT 1 FROM task_delegation_notifications n
        JOIN task_delegation_responses r ON r.notification_id=n.id
        JOIN ({OUTCOMES}) outcome ON outcome.child_id=n.child_task_id
          AND outcome.parent_id=n.parent_task_id AND outcome.attempt_id=n.child_attempt_id
          AND outcome.outcome=n.child_status
        WHERE n.parent_task_id=t.id AND r.response_attempt_id=q.attempt_id
          AND t.agent_id=q.agent_id AND n.parent_agent_id=q.agent_id AND candidate.session_id=q.agent_id
          AND NOT EXISTS (SELECT 1 FROM connect_work w WHERE w.kind='task_turn' AND w.local_id=t.id)
    )"
    )
}

/// Runs with the dispatch writer lock. Notifications, prompt messages and their
/// response attempt commit together; retries and restart scans are idempotent.
pub(super) fn reconcile_in_transaction(tx: &Transaction<'_>) -> Result<()> {
    let mut stmt = tx.prepare(&format!(
        "SELECT outcome.child_id, outcome.parent_id, outcome.title, outcome.agent_id,
                outcome.attempt_id, outcome.outcome, outcome.error_message, parent.agent_id
         FROM ({OUTCOMES}) outcome
         JOIN tasks parent ON parent.id=outcome.parent_id
         JOIN agents owner ON owner.id=parent.agent_id AND owner.project_id=parent.project_id
         WHERE outcome.outcome IS NOT NULL
           AND parent.status NOT IN ('completed','cancelled') AND parent.hidden=0
           AND parent.provenance!='native_plan'
           AND NOT EXISTS (SELECT 1 FROM projects p WHERE p.id=parent.project_id
               AND p.deletion_started_at IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM connect_work w WHERE w.kind='task_turn' AND w.local_id=parent.id)
           AND NOT EXISTS (SELECT 1 FROM task_delegation_notifications n
               WHERE n.parent_task_id=parent.id AND n.parent_agent_id=parent.agent_id AND n.child_task_id=outcome.child_id
                 AND n.child_attempt_id=outcome.attempt_id AND n.child_status=outcome.outcome)
         ORDER BY parent.id, outcome.child_id"
    ))?;
    let notifications = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (child, parent, title, agent, child_attempt, status, error, owner) in notifications {
        let details = serde_json::json!({
            "task_id": child, "title": title, "agent_id": agent, "status": status,
            "error": error, "attempt_id": child_attempt,
        });
        let content = format!(
            "Delegated task needs attention: {title}\nTask: {child}\nAgent: {}\nStatus: {status}\n{}\n\nThe parent has been notified. Required delegated work still needs to be resolved before completion.",
            agent.as_deref().unwrap_or("Unassigned"),
            error.as_deref().unwrap_or("Open the child task for details."),
        );
        let message =
            TaskConversation::insert_text_message_in_transaction(tx, &parent, "system", &content)?;
        let response = TaskQueue::enqueue_delegation_notification_in_transaction(
            tx,
            &parent,
            &owner,
            message.id,
            &message.timestamp,
        )?;
        tx.execute(
            "INSERT INTO task_delegation_notifications
             (parent_task_id, child_task_id, child_attempt_id, child_status, parent_agent_id, message_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![parent, child, child_attempt, status, owner, message.id],
        )?;
        let notification_id = tx.last_insert_rowid();
        tx.execute("INSERT INTO task_delegation_responses(response_attempt_id,notification_id) VALUES(?1,?2)", params![response,notification_id])?;
        tx.execute(
            "INSERT INTO session_events
             (session_id, attempt_id, task_id, source_type, source_id, event_type, summary, payload)
             VALUES (?1, ?2, ?3, 'task', ?3, 'delegation_attention', ?4, ?5)",
            params![
                owner,
                response,
                parent,
                format!("Delegated task needs attention: {title}"),
                details.to_string()
            ],
        )?;
    }
    // After the parent reports a blocker, a reply, explicit resume or scheduled check must
    // also be able to handle it. Reuse the journal without creating a new message
    // or any new work. Workflow-owned responses retain their exclusive prompt.
    tx.execute(&format!("INSERT OR IGNORE INTO task_delegation_responses(response_attempt_id,notification_id)
        SELECT q.attempt_id,n.id FROM task_delegation_notifications n
        JOIN tasks t ON t.id=n.parent_task_id AND t.agent_id=n.parent_agent_id
        JOIN ({OUTCOMES}) outcome ON outcome.child_id=n.child_task_id
          AND outcome.parent_id=t.id AND outcome.attempt_id=n.child_attempt_id AND outcome.outcome=n.child_status
        JOIN task_queue q ON q.task_id=t.id AND q.agent_id=t.agent_id AND q.status='queued'
        JOIN work_attempts a ON a.id=q.attempt_id AND a.session_id=t.agent_id AND a.status='queued'
        WHERE t.status NOT IN ('completed','cancelled')
          AND NOT EXISTS (SELECT 1 FROM connect_work w WHERE w.kind='task_turn' AND w.local_id=t.id)
          AND NOT EXISTS (SELECT 1 FROM workflow_step_executions e WHERE e.continuation_attempt_id=a.id)"), [])?;
    Ok(())
}

/// Only journal-owned system messages can supply this recovery context. A
/// missing/deleted message must fail closed instead of rerunning the task brief.
pub(crate) fn prompt(db: &Database, task_id: &str, attempt_id: &str) -> Result<Option<String>> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT m.content FROM task_delegation_notifications n
             JOIN task_delegation_responses r ON r.notification_id=n.id
             LEFT JOIN task_messages m ON m.id=n.message_id AND m.task_id=n.parent_task_id AND m.role='system'
             WHERE n.parent_task_id=?1 AND r.response_attempt_id=?2 ORDER BY n.id"
        )?;
        let rows = stmt.query_map(params![task_id, attempt_id], |row| row.get::<_, Option<String>>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() { return Ok(None); }
        let content = rows.into_iter().collect::<Option<Vec<_>>>().ok_or_else(||
            Error::Task("Delegation recovery is missing its notification message".into()))?;
        Ok(Some(format!("A delegated task reported a problem. Handle this notification in the existing parent task. Inspect each child's latest state before acting. Diagnose the cause; do not blindly retry or create replacement tasks. If safe recovery needs user input or an environment/settings change, report the blocker and ask the user. Keep required reviews and completion gates intact. Do not restart the parent's original work just to handle this notification.\n\n{}", content.join("\n\n"))))
    })
}

pub(crate) fn is_recovery(db: &Database, attempt_id: &str) -> Result<bool> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT 1 FROM task_delegation_responses WHERE response_attempt_id=?1 LIMIT 1",
            [attempt_id],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .map_err(Error::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agents::registry::AgentRegistry,
        sessions::SessionManager,
        tasks::board::{CreateTask, Task, TaskBoard},
    };
    use std::sync::Arc;

    fn setup() -> (Arc<Database>, TaskBoard, TaskQueue, Task, Task) {
        setup_db(Arc::new(Database::open_memory().unwrap()))
    }
    fn setup_db(db: Arc<Database>) -> (Arc<Database>, TaskBoard, TaskQueue, Task, Task) {
        for agent in ["parent", "reviewer"] {
            AgentRegistry::new(db.clone())
                .ensure(agent, "native")
                .unwrap();
        }
        db.conn()
            .execute(
                "UPDATE agents SET project_id='parent' WHERE id='reviewer'",
                [],
            )
            .unwrap();
        let board = TaskBoard::new(db.clone());
        let parent = board
            .create(&CreateTask {
                title: "Benchmark already finished".into(),
                agent_id: Some("parent".into()),
                ..Default::default()
            })
            .unwrap();
        let child = board
            .create(&CreateTask {
                title: "Required review".into(),
                agent_id: Some("reviewer".into()),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let queue = TaskQueue::new(db.clone());
        (db, board, queue, parent, child)
    }
    fn fail(db: &Arc<Database>, board: &TaskBoard, queue: &TaskQueue, child: &Task) -> String {
        let item = queue
            .enqueue(&child.id, child.agent_id.as_deref().unwrap())
            .unwrap();
        let attempt = item.attempt_id.unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                &attempt,
                "failed",
                "Environment mismatch",
                None,
                Some("Retained environment needs different container settings"),
            )
            .unwrap();
        queue.fail(item.id, "Environment mismatch").unwrap();
        board.update_status(&child.id, "blocked", None).unwrap();
        attempt
    }
    fn count(db: &Database, table: &str) -> i64 {
        db.conn()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn reconcile(db: &Database) {
        db.with_conn(|conn| -> Result<()> {
            let tx = conn.unchecked_transaction()?;
            reconcile_in_transaction(&tx)?;
            tx.commit()?;
            Ok(())
        })
        .unwrap();
    }
    fn finish(db: &Arc<Database>, queue: &TaskQueue, item: &super::super::queue::QueueItem) {
        SessionManager::new(db.clone())
            .transition_attempt(
                item.attempt_id.as_deref().unwrap(),
                "completed",
                "Reported blocker",
                None,
                None,
            )
            .unwrap();
        queue.complete(item.id, "Reported blocker").unwrap();
    }

    #[test]
    fn failed_delegation_wakes_parent_once_and_preserves_completion_gate() {
        for by_agent in [true, false] {
            let (db, board, queue, parent, child) = setup();
            let saved = TaskConversation::new(db.clone())
                .add_user_message_with_attachments_and_enqueue(
                    &parent.id,
                    Some("parent"),
                    "Benchmark finished; inspect results",
                    &[],
                )
                .unwrap();
            let queued_before: i64 = db
                .conn()
                .query_row(
                    "SELECT id FROM task_queue WHERE task_id=?1",
                    [&parent.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(queue.claim("parent").unwrap().is_none());
            fail(&db, &board, &queue, &child);
            // Failure occurred before any native session was started.
            let claimed = if by_agent {
                queue.claim("parent")
            } else {
                queue.claim_next()
            }
            .unwrap()
            .unwrap();
            assert_eq!(claimed.id, queued_before, "Reuse the saved wake-up");
            let recovery = prompt(&db, &parent.id, claimed.attempt_id.as_deref().unwrap())
                .unwrap()
                .unwrap();
            assert!(recovery.contains(&child.id));
            assert!(recovery.contains("container settings"));
            assert!(recovery.contains("Required review"));
            assert!(!board.subtasks_complete(&parent.id).unwrap());
            assert!(board
                .complete_and_roll_up(&parent.id, Some("parent"))
                .unwrap()
                .is_empty());
            finish(&db, &queue, &claimed);
            for _ in 0..3 {
                assert!(TaskQueue::new(db.clone()).claim_next().unwrap().is_none());
            }
            assert_eq!(count(&db, "task_delegation_notifications"), 1);
            assert_eq!(count(&db, "task_messages"), 2);
            let messages = TaskConversation::new(db.clone())
                .get_messages(&parent.id)
                .unwrap();
            assert_eq!(messages[0].id, saved.0.id);
            assert_eq!(messages[1].role, "system");
            // A real new child attempt failing is a new outcome, not a poll replay.
            fail(&db, &board, &queue, &child);
            assert!(queue.claim("parent").unwrap().is_some());
            assert_eq!(count(&db, "task_delegation_notifications"), 2);
        }
    }

    #[test]
    fn several_child_failures_coalesce_without_bypassing_agent_ownership() {
        let (db, board, queue, parent, child) = setup();
        let initial = queue.enqueue(&parent.id, "parent").unwrap();
        // Model an already-running parent creating children during its turn.
        db.conn()
            .execute(
                "UPDATE task_queue SET status='running' WHERE id=?1",
                [initial.id],
            )
            .unwrap();
        SessionManager::new(db.clone())
            .transition_attempt(
                initial.attempt_id.as_deref().unwrap(),
                "running",
                "Working",
                None,
                None,
            )
            .unwrap();
        fail(&db, &board, &queue, &child);
        let second = board
            .create(&CreateTask {
                title: "Second review".into(),
                agent_id: Some("reviewer".into()),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        fail(&db, &board, &queue, &second);
        assert!(queue.claim("parent").unwrap().is_none());
        assert_eq!(count(&db, "task_delegation_notifications"), 2);
        let responses: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(DISTINCT response_attempt_id) FROM task_delegation_responses",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(responses, 1);
        finish(&db, &queue, &initial);
        let recovery = queue.claim("parent").unwrap().unwrap();
        let text = prompt(&db, &parent.id, recovery.attempt_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert!(text.contains(&child.id) && text.contains(&second.id));
    }

    #[test]
    fn recovery_does_not_override_scheduling_dependencies_or_terminal_parent() {
        for constraint in [
            "backlog",
            "scheduled",
            "dependency",
            "completed",
            "cancelled",
            "deleting",
            "connect",
            "wrong_agent",
        ] {
            let (db, board, queue, parent, child) = setup();
            fail(&db, &board, &queue, &child);
            match constraint {
                "backlog" => {
                    db.conn()
                        .execute("UPDATE tasks SET backlog=1 WHERE id=?1", [&parent.id])
                        .unwrap();
                }
                "scheduled" => {
                    db.conn()
                        .execute(
                            "UPDATE tasks SET start_after='2999-01-01T00:00:00Z' WHERE id=?1",
                            [&parent.id],
                        )
                        .unwrap();
                }
                "dependency" => {
                    let dep = board
                        .create(&CreateTask {
                            title: "Prerequisite".into(),
                            agent_id: Some("parent".into()),
                            ..Default::default()
                        })
                        .unwrap();
                    board.add_dependency(&parent.id, &dep.id).unwrap();
                }
                "completed" | "cancelled" => {
                    db.conn()
                        .execute(
                            "UPDATE tasks SET status=?1 WHERE id=?2",
                            params![constraint, parent.id],
                        )
                        .unwrap();
                }
                "deleting" => {
                    db.conn().execute("UPDATE projects SET deletion_started_at=CURRENT_TIMESTAMP WHERE id='parent'",[]).unwrap();
                }
                "connect" => {
                    // The real Connect authority fixture is also covered by the Connect suite.
                    db.conn().execute_batch("INSERT INTO connect_bindings(id,instance_id,local_project_id,local_agent_id,generation,active,binding_json) VALUES('binding','instance','parent','parent',1,1,'{}')").unwrap();
                    db.conn().execute("INSERT INTO connect_work(instance_id,binding_id,kind,work_id,local_id) VALUES('instance','binding','task_turn','remote',?1)",[&parent.id]).unwrap();
                }
                "wrong_agent" => {
                    db.conn()
                        .execute(
                            "UPDATE agents SET project_id='reviewer' WHERE id='parent'",
                            [],
                        )
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(queue.claim("parent").unwrap().is_none(), "{constraint}");
            if matches!(
                constraint,
                "completed" | "cancelled" | "deleting" | "connect" | "wrong_agent"
            ) {
                assert_eq!(
                    count(&db, "task_delegation_notifications"),
                    0,
                    "{constraint}"
                );
            }
        }
    }

    #[test]
    fn resolved_or_retried_child_invalidates_the_exception_but_history_remains() {
        let (db, board, queue, parent, child) = setup();
        fail(&db, &board, &queue, &child);
        reconcile(&db);
        queue.enqueue(&child.id, "reviewer").unwrap();
        // Original raw blocked state can remain while a retry is queued.
        assert!(queue.claim("parent").unwrap().is_none());
        assert_eq!(count(&db, "task_delegation_notifications"), 1);
        assert_eq!(count(&db, "task_messages"), 1);
        assert!(!board.subtasks_complete(&parent.id).unwrap());
    }

    #[test]
    fn cancelled_and_input_waiting_children_notify_but_plan_rows_do_not() {
        for state in ["cancelled", "waiting_for_input", "blocked"] {
            let (db, board, queue, parent, child) = setup();
            board.update_status(&child.id, state, None).unwrap();
            let recovery = queue.claim("parent").unwrap().unwrap();
            assert_eq!(recovery.task_id, parent.id);
            assert!(
                prompt(&db, &parent.id, recovery.attempt_id.as_deref().unwrap())
                    .unwrap()
                    .unwrap()
                    .contains(state)
            );
        }
        let (db, board, queue, _, child) = setup();
        db.conn()
            .execute(
                "UPDATE tasks SET provenance='native_plan' WHERE id=?1",
                [&child.id],
            )
            .unwrap();
        board.update_status(&child.id, "blocked", None).unwrap();
        assert!(queue.claim_next().unwrap().is_none());
        assert_eq!(count(&db, "task_delegation_notifications"), 0);
    }
    #[test]
    fn notification_queue_and_message_are_atomic_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let (db, board, queue, parent, child) = setup_db(Arc::new(Database::open(&path).unwrap()));
        fail(&db, &board, &queue, &child);
        db.conn().execute_batch("CREATE TRIGGER abort_notification BEFORE INSERT ON task_delegation_notifications BEGIN SELECT RAISE(ABORT, 'interrupted transaction'); END;").unwrap();
        assert!(queue.claim_next().is_err());
        assert_eq!(count(&db, "task_messages"), 0);
        assert_eq!(count(&db, "task_queue"), 1);
        db.conn()
            .execute_batch("DROP TRIGGER abort_notification")
            .unwrap();
        reconcile(&db);
        drop(queue);
        drop(board);
        drop(db);
        let db = Arc::new(Database::open(&path).unwrap());
        let queue = TaskQueue::new(db.clone());
        let item = queue.claim("parent").unwrap().unwrap();
        assert_eq!(item.task_id, parent.id);
        assert_eq!(count(&db, "task_messages"), 1);
        assert_eq!(count(&db, "task_delegation_notifications"), 1);
        finish(&db, &queue, &item);
        assert!(queue.claim_next().unwrap().is_none());
    }

    #[test]
    fn stale_assignment_and_cancelled_parent_cannot_use_queued_recovery_grant() {
        for change in ["assignment", "cancel"] {
            let (db, board, queue, parent, child) = setup();
            fail(&db, &board, &queue, &child);
            reconcile(&db);
            if change == "assignment" {
                db.conn()
                    .execute(
                        "UPDATE tasks SET agent_id='reviewer' WHERE id=?1",
                        [&parent.id],
                    )
                    .unwrap();
            } else {
                board.update_status(&parent.id, "cancelled", None).unwrap();
            }
            assert!(queue.claim("parent").unwrap().is_none());
            assert_eq!(
                count(&db, "task_delegation_notifications"),
                if change == "assignment" { 2 } else { 1 }
            );
            if change == "assignment" {
                assert!(queue.claim("reviewer").unwrap().is_some());
            }
        }
    }

    #[test]
    fn child_deletion_does_not_remove_recovery_prompt_identity() {
        let (db, board, queue, parent, child) = setup();
        fail(&db, &board, &queue, &child);
        reconcile(&db);
        board.delete(&child.id).unwrap();
        let item = queue.claim("parent").unwrap().unwrap();
        assert!(prompt(&db, &parent.id, item.attempt_id.as_deref().unwrap())
            .unwrap()
            .unwrap()
            .contains(&child.id));
    }

    #[test]
    fn user_reply_after_recovery_can_handle_the_same_blocker_without_another_alert() {
        let (db, board, queue, parent, child) = setup();
        fail(&db, &board, &queue, &child);
        let initial = queue.claim("parent").unwrap().unwrap();
        finish(&db, &queue, &initial);
        assert!(queue.claim("parent").unwrap().is_none());
        TaskConversation::new(db.clone())
            .add_user_message_with_attachments_and_enqueue(
                &parent.id,
                Some("parent"),
                "Settings restored; retry that review",
                &[],
            )
            .unwrap();
        let response = queue.claim("parent").unwrap().unwrap();
        assert_ne!(initial.id, response.id);
        assert!(
            prompt(&db, &parent.id, response.attempt_id.as_deref().unwrap())
                .unwrap()
                .unwrap()
                .contains(&child.id)
        );
        finish(&db, &queue, &response);
        assert!(queue.claim("parent").unwrap().is_none());
        assert_eq!(count(&db, "task_delegation_notifications"), 1);
        assert_eq!(count(&db, "task_delegation_responses"), 2);
        assert_eq!(count(&db, "task_messages"), 2);
        // Explicit resume can queue a turn without a new message too.
        let resumed = queue.enqueue(&parent.id, "parent").unwrap();
        assert_eq!(queue.claim("parent").unwrap().unwrap().id, resumed.id);
        assert!(is_recovery(&db, resumed.attempt_id.as_deref().unwrap()).unwrap());
        finish(&db, &queue, &resumed);
        assert!(queue.claim("parent").unwrap().is_none());
        assert_eq!(count(&db, "task_delegation_notifications"), 1);
    }
}
