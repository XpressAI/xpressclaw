use super::*;
use crate::{
    agents::registry::AgentRegistry,
    sessions::SessionManager,
    tasks::board::{CreateTask, TaskStatus},
};

fn setup() -> (Arc<Database>, TaskBoard, TaskQueue, Task, Task) {
    setup_db(Arc::new(Database::open_memory().unwrap()))
}

fn setup_db(db: Arc<Database>) -> (Arc<Database>, TaskBoard, TaskQueue, Task, Task) {
    let agents = AgentRegistry::new(db.clone());
    for agent in ["manager", "worker", "peer", "outside"] {
        agents.ensure(agent, "native").unwrap();
    }
    db.conn()
        .execute(
            "UPDATE agents SET project_id='manager' WHERE id IN ('worker','peer')",
            [],
        )
        .unwrap();
    let board = TaskBoard::new(db.clone());
    let queue = TaskQueue::new(db.clone());
    let parent = board
        .create(&CreateTask {
            title: "Coordinate rollout".into(),
            agent_id: Some("manager".into()),
            ..Default::default()
        })
        .unwrap();
    let child = board
        .create(&CreateTask {
            title: "Deploy".into(),
            agent_id: Some("worker".into()),
            parent_task_id: Some(parent.id.clone()),
            ..Default::default()
        })
        .unwrap();
    (db, board, queue, parent, child)
}

#[test]
fn upgrade_preserves_notification_receipts_and_requires_review_of_open_delegations() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.db");
    let (db, board, queue, parent, child) = setup_db(Arc::new(Database::open(&path).unwrap()));
    let historical = board
        .create(&CreateTask {
            title: "Previously accepted".into(),
            agent_id: Some("worker".into()),
            parent_task_id: Some(parent.id.clone()),
            ..Default::default()
        })
        .unwrap();
    board
        .update_status(&historical.id, "completed", None)
        .unwrap();
    let work = queue.enqueue(&child.id, "worker").unwrap();
    SessionManager::new(db.clone())
        .transition_attempt(
            work.attempt_id.as_deref().unwrap(),
            "failed",
            "Login expired",
            None,
            Some("Refresh login"),
        )
        .unwrap();
    queue.fail(work.id, "Login expired").unwrap();
    board.update_status(&child.id, "blocked", None).unwrap();
    db.with_conn(|conn| {
        let tx = conn.unchecked_transaction().unwrap();
        delegation_notifications::reconcile_in_transaction(&tx).unwrap();
        tx.commit().unwrap();
    });
    let receipt: (i64,i64,String) = db.conn().query_row(
        "SELECT n.id,n.message_id,r.response_attempt_id FROM task_delegation_notifications n JOIN task_delegation_responses r ON r.notification_id=n.id",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).unwrap();
    db.conn()
        .execute_batch(
            "ALTER TABLE tasks DROP COLUMN parent_review_required;
        ALTER TABLE task_delegation_notifications DROP COLUMN decision;
        ALTER TABLE task_delegation_notifications DROP COLUMN review_note;
        ALTER TABLE task_delegation_notifications DROP COLUMN reviewed_at;
        UPDATE config SET value='55' WHERE key='schema_version';",
        )
        .unwrap();
    drop(queue);
    drop(board);
    drop(db);
    let db = Arc::new(Database::open(&path).unwrap());
    let board = TaskBoard::new(db.clone());
    assert!(board.get(&child.id).unwrap().parent_review_required);
    assert!(!board.get(&historical.id).unwrap().parent_review_required);
    assert_eq!(
        board.get(&historical.id).unwrap().status,
        TaskStatus::Completed
    );
    let restored: (i64,i64,String) = db.conn().query_row(
        "SELECT n.id,n.message_id,r.response_attempt_id FROM task_delegation_notifications n JOIN task_delegation_responses r ON r.notification_id=n.id",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).unwrap();
    assert_eq!(receipt, restored);
    assert!(db
        .conn()
        .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
        .optional()
        .unwrap()
        .is_none());
    review(
        db.clone(),
        "manager",
        &child.id,
        &request(
            &board,
            &parent,
            &child,
            work.attempt_id.as_deref().unwrap(),
            ReviewDecision::Wait,
        ),
    )
    .unwrap();
    assert_eq!(board.get(&child.id).unwrap().status, TaskStatus::Blocked);
}

#[test]
fn deleting_parent_retains_child_as_independent_unfinished_work() {
    for bulk in [false, true] {
        let (db, board, queue, parent, child) = setup();
        finish_child(&db, &board, &queue, &child, "Waiting for authorization");
        if bulk {
            board.delete_by_status("pending").unwrap();
        } else {
            board.delete(&parent.id).unwrap();
        }
        let preserved = board.get(&child.id).unwrap();
        assert!(preserved.parent_task_id.is_none());
        assert!(!preserved.parent_review_required);
        assert_eq!(preserved.status, TaskStatus::InProgress);
    }
}

#[test]
fn cancelling_parent_through_board_releases_surviving_child_review_gates() {
    assert_parent_cancellation_releases_reviews(false);
}

#[test]
fn cancelling_parent_through_sessions_releases_surviving_child_review_gates() {
    assert_parent_cancellation_releases_reviews(true);
}

fn assert_parent_cancellation_releases_reviews(cancel_attempts: bool) {
    for state in ["running", "finished", "held"] {
        let (db, board, queue, parent, child) = setup();
        let sessions = SessionManager::new(db.clone());
        let historical = board
            .create(&CreateTask {
                title: "Previously accepted work".into(),
                agent_id: Some("worker".into()),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        board
            .update_status(&historical.id, "completed", None)
            .unwrap();
        let dependent = board
            .create(&CreateTask {
                title: "Verify the completed deployment".into(),
                agent_id: Some("peer".into()),
                ..Default::default()
            })
            .unwrap();
        board.add_dependency(&dependent.id, &child.id).unwrap();
        queue.enqueue(&dependent.id, "peer").unwrap();
        queue.enqueue(&child.id, "worker").unwrap();
        let response = queue.claim("worker").unwrap().unwrap();
        let attempt = response.attempt_id.as_deref().unwrap();
        board
            .update_status(&child.id, "in_progress", Some("worker"))
            .unwrap();
        if state != "running" {
            sessions
                .transition_attempt(
                    attempt,
                    "completed",
                    "Worker finished",
                    Some("Waiting for deployment authorization"),
                    None,
                )
                .unwrap();
            queue
                .complete(response.id, "Waiting for deployment authorization")
                .unwrap();
            assert!(board
                .complete_and_roll_up(&child.id, None)
                .unwrap()
                .is_empty());
        }
        if state == "held" {
            review(
                db.clone(),
                "manager",
                &child.id,
                &request(&board, &parent, &child, attempt, ReviewDecision::Wait),
            )
            .unwrap();
        }
        let before = board.get(&child.id).unwrap();
        let worker = sessions.get_attempt(attempt).unwrap();
        assert!(before.parent_review_required);

        if cancel_attempts {
            sessions
                .cancel_task_attempts(&parent.id, "Cancel coordination")
                .unwrap()
                .unwrap();
        } else {
            board.update_status(&parent.id, "cancelled", None).unwrap();
        }

        let independent = board.get(&child.id).unwrap();
        assert!(!independent.parent_review_required, "{state}");
        assert!(independent.parent_task_id.is_none());
        assert_eq!(independent.status, before.status);
        assert!(independent.completed_at.is_none());
        assert_eq!(sessions.get_attempt(attempt).unwrap().status, worker.status);
        assert_eq!(sessions.get_attempt(attempt).unwrap().result, worker.result);
        assert_eq!(
            board.get(&historical.id).unwrap().parent_task_id,
            Some(parent.id.clone())
        );
        assert!(queue.claim("worker").unwrap().is_none());
        assert!(queue.claim("peer").unwrap().is_none());
        assert!(queue.claim("manager").unwrap().is_none());

        if state == "running" {
            sessions
                .transition_attempt(
                    attempt,
                    "completed",
                    "Deployment finished",
                    Some("Deployed and verified"),
                    None,
                )
                .unwrap();
            queue
                .complete(response.id, "Deployed and verified")
                .unwrap();
            assert_eq!(
                board.complete_and_roll_up(&child.id, None).unwrap().len(),
                1
            );
        } else {
            // An independent result can now be explicitly completed without
            // requiring acceptance by the cancelled parent.
            board.update_status(&child.id, "completed", None).unwrap();
        }
        assert_eq!(board.get(&child.id).unwrap().status, TaskStatus::Completed);
        assert_eq!(queue.claim("peer").unwrap().unwrap().task_id, dependent.id);
        assert_eq!(board.get(&parent.id).unwrap().status, TaskStatus::Cancelled);
    }
}

#[test]
fn cancelling_parent_preserves_its_surviving_childs_own_review_authority() {
    let (db, board, queue, parent, child) = setup();
    queue.enqueue(&child.id, "worker").unwrap();
    queue.claim("worker").unwrap().unwrap();
    let nested = board
        .create(&CreateTask {
            title: "Verify a deployment prerequisite".into(),
            agent_id: Some("peer".into()),
            parent_task_id: Some(child.id.clone()),
            ..Default::default()
        })
        .unwrap();
    queue.enqueue(&nested.id, "peer").unwrap();
    let response = queue.claim("peer").unwrap().unwrap();
    let attempt = response.attempt_id.as_deref().unwrap();

    board.update_status(&parent.id, "cancelled", None).unwrap();

    assert!(!board.get(&child.id).unwrap().parent_review_required);
    let owned = board.get(&nested.id).unwrap();
    assert!(owned.parent_review_required);
    assert_eq!(owned.parent_task_id, Some(child.id.clone()));
    SessionManager::new(db.clone())
        .transition_attempt(
            attempt,
            "completed",
            "Verified",
            Some("Prerequisite verified"),
            None,
        )
        .unwrap();
    queue
        .complete(response.id, "Prerequisite verified")
        .unwrap();
    assert!(board
        .complete_and_roll_up(&nested.id, None)
        .unwrap()
        .is_empty());
    let (accepted, _) = review(
        db,
        "worker",
        &nested.id,
        &request(&board, &child, &nested, attempt, ReviewDecision::Accept),
    )
    .unwrap();
    assert_eq!(accepted.status, TaskStatus::Completed);
}

#[test]
fn many_completed_workers_coalesce_without_flooding_the_parent_prompt() {
    let (db, board, queue, parent, first) = setup();
    let output = "Detailed worker evidence. ".repeat(300);
    finish_child(&db, &board, &queue, &first, &output);
    for index in 1..25 {
        let child = board
            .create(&CreateTask {
                title: format!("Milestone {index}"),
                agent_id: Some("worker".into()),
                parent_task_id: Some(parent.id.clone()),
                ..Default::default()
            })
            .unwrap();
        finish_child(&db, &board, &queue, &child, &output);
    }
    let queued: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM task_queue WHERE task_id=?1 AND status='queued'",
            [&parent.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1);
    let response = queue.claim("manager").unwrap().unwrap();
    let text =
        delegation_notifications::prompt(&db, &parent.id, response.attempt_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
    assert!(text.contains("5 additional delegated reports"));
    assert!(text.len() < 64_000);
    let recorded: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM task_delegation_notifications WHERE parent_task_id=?1",
            [&parent.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, 25);
    assert_eq!(
        board
            .list_subtasks(&parent.id)
            .unwrap()
            .iter()
            .filter(|task| task.parent_review_required)
            .count(),
        25
    );
}

fn finish_child(
    db: &Arc<Database>,
    board: &TaskBoard,
    queue: &TaskQueue,
    child: &Task,
    output: &str,
) -> String {
    queue.enqueue(&child.id, "worker").unwrap();
    let item = queue.claim("worker").unwrap().unwrap();
    board
        .update_status(&child.id, "in_progress", Some("worker"))
        .unwrap();
    let attempt = item.attempt_id.unwrap();
    SessionManager::new(db.clone())
        .transition_attempt(
            &attempt,
            "completed",
            "Response finished",
            Some(output),
            None,
        )
        .unwrap();
    queue.complete(item.id, output).unwrap();
    assert!(board
        .complete_and_roll_up(&child.id, Some("worker"))
        .unwrap()
        .is_empty());
    attempt
}

fn request(
    board: &TaskBoard,
    parent: &Task,
    child: &Task,
    attempt: &str,
    decision: ReviewDecision,
) -> ReviewRequest {
    ReviewRequest {
        parent_task_id: parent.id.clone(),
        expected_attempt_id: attempt.into(),
        expected_revision: board.get(&child.id).unwrap().revision,
        decision,
        guidance: Some("Complete the missing deployment after the login refresh.".into()),
    }
}

#[test]
fn parent_can_hold_an_incomplete_result_previously_marked_completed() {
    let (db, board, queue, parent, child) = setup();
    let attempt = finish_child(
        &db,
        &board,
        &queue,
        &child,
        "D1 remains incomplete. Waiting for login refresh.",
    );
    db.conn().execute("UPDATE tasks SET parent_review_required=0,status='completed',completed_at=CURRENT_TIMESTAMP WHERE id=?1", [&child.id]).unwrap();
    let hold = request(&board, &parent, &child, &attempt, ReviewDecision::Wait);
    let (held, _) = review(db.clone(), "manager", &child.id, &hold).unwrap();
    assert_eq!(held.status, TaskStatus::Blocked);
    assert!(held.parent_review_required);
    assert!(held.completed_at.is_none());
    assert!(!board.subtasks_complete(&parent.id).unwrap());
    assert!(
        queue.claim("worker").unwrap().is_none(),
        "waiting records the blocker without retrying"
    );
    assert!(
        queue.claim("manager").unwrap().is_none(),
        "acknowledging a wait does not create another notification"
    );
}

#[test]
fn incomplete_response_wakes_parent_without_completing_child_or_releasing_dependencies() {
    let (db, board, queue, parent, child) = setup();
    let sibling = board
        .create(&CreateTask {
            title: "Other running work".into(),
            agent_id: Some("peer".into()),
            parent_task_id: Some(parent.id.clone()),
            ..Default::default()
        })
        .unwrap();
    let dependent = board
        .create(&CreateTask {
            title: "Verify deployment".into(),
            agent_id: Some("peer".into()),
            ..Default::default()
        })
        .unwrap();
    board.add_dependency(&dependent.id, &child.id).unwrap();
    queue.enqueue(&dependent.id, "peer").unwrap();
    let attempt = finish_child(
        &db,
        &board,
        &queue,
        &child,
        "D1 remains incomplete. No deployment occurred. Waiting for AWS login refresh.",
    );
    let saved = board.get(&child.id).unwrap();
    assert_ne!(saved.status, TaskStatus::Completed);
    assert!(saved.parent_review_required);
    assert!(board.update_status(&child.id, "completed", None).is_err());
    assert!(queue.claim("peer").unwrap().is_none());
    let response = queue
        .claim("manager")
        .unwrap()
        .expect("finished child wakes manager despite another pending child");
    let prompt =
        delegation_notifications::prompt(&db, &parent.id, response.attempt_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
    assert!(prompt.contains("No deployment occurred"));
    assert!(prompt.contains("review_delegated_task"));
    assert!(prompt.contains(&attempt));
    assert_eq!(board.get(&sibling.id).unwrap().status, TaskStatus::Pending);
    review(
        db.clone(),
        "manager",
        &child.id,
        &request(&board, &parent, &child, &attempt, ReviewDecision::Wait),
    )
    .unwrap();
    assert_eq!(board.get(&child.id).unwrap().status, TaskStatus::Blocked);
    assert!(queue.claim("peer").unwrap().is_none());
}

#[test]
fn acceptance_is_parent_scoped_current_and_releases_dependencies() {
    let (db, board, queue, parent, child) = setup();
    let dependent = board
        .create(&CreateTask {
            title: "Verify accepted deployment".into(),
            agent_id: Some("peer".into()),
            ..Default::default()
        })
        .unwrap();
    board.add_dependency(&dependent.id, &child.id).unwrap();
    queue.enqueue(&dependent.id, "peer").unwrap();
    let attempt = finish_child(
        &db,
        &board,
        &queue,
        &child,
        "Deployment verified and evidence saved.",
    );
    let mut approval = request(&board, &parent, &child, &attempt, ReviewDecision::Accept);
    assert!(review(db.clone(), "worker", &child.id, &approval).is_err());
    assert!(review(db.clone(), "peer", &child.id, &approval).is_err());
    assert!(review(db.clone(), "outside", &child.id, &approval).is_err());
    board
        .update(
            &child.id,
            &crate::tasks::board::UpdateTask {
                title: None,
                description: Some("Verify against the updated deployment requirements".into()),
                agent_id: None,
                priority: None,
            },
        )
        .unwrap();
    assert!(review(db.clone(), "manager", &child.id, &approval)
        .unwrap_err()
        .to_string()
        .contains("revision"));
    approval.expected_revision = board.get(&child.id).unwrap().revision;
    let mut stale = request(
        &board,
        &parent,
        &child,
        "old-attempt",
        ReviewDecision::Accept,
    );
    assert!(review(db.clone(), "manager", &child.id, &stale).is_err());
    let (accepted, _) = review(db.clone(), "manager", &child.id, &approval).unwrap();
    assert_eq!(accepted.status, TaskStatus::Completed);
    assert!(!accepted.parent_review_required);
    assert_eq!(queue.claim("peer").unwrap().unwrap().task_id, dependent.id);
    assert!(board.subtasks_complete(&parent.id).unwrap());
    stale.expected_attempt_id = attempt;
    assert!(review(db.clone(), "manager", &child.id, &stale).is_ok());
    assert!(
        queue.claim("manager").unwrap().is_none(),
        "acceptance retires an unneeded automatic notification"
    );
}

#[test]
fn user_messages_and_scheduled_checks_resume_parents_with_pending_children() {
    use crate::tasks::scheduler::{CreateOneShotSchedule, ScheduleManager};
    for scheduled in [false, true] {
        let (db, board, queue, parent, child) = setup();
        if scheduled {
            let manager = ScheduleManager::new(db.clone());
            let wakeup = manager
                .create_one_shot(&CreateOneShotSchedule {
                    name: "Coordinate existing work".into(),
                    run_at: None,
                    delay_seconds: Some(60),
                    agent_id: "manager".into(),
                    title: "Check progress".into(),
                    description: None,
                    continuation_task_id: Some(parent.id.clone()),
                    conversation_id: None,
                })
                .unwrap();
            manager.trigger(&wakeup.id, &board).unwrap();
            assert_eq!(manager.get(&wakeup.id).unwrap().run_count, 1);
        } else {
            TaskConversation::new(db.clone())
                .add_user_message_with_attachments_and_enqueue(
                    &parent.id,
                    Some("manager"),
                    "Coordinate the children without restarting their work",
                    &[],
                )
                .unwrap();
        }
        assert_eq!(
            board
                .activity_status(&board.get(&parent.id).unwrap())
                .unwrap(),
            "queued"
        );
        assert_eq!(
            crate::tasks::planning::TaskPlanner::new(db.clone())
                .get(&parent.id)
                .unwrap()
                .planning
                .lane,
            "queue"
        );
        assert_eq!(queue.claim("manager").unwrap().unwrap().task_id, parent.id);
        assert_eq!(board.get(&child.id).unwrap().status, TaskStatus::Pending);
        assert!(!board.subtasks_complete(&parent.id).unwrap());
        assert!(queue.claim("manager").unwrap().is_none());
    }
}

#[test]
fn coordination_keeps_backlog_dates_dependencies_and_terminal_constraints() {
    for constraint in ["backlog", "future", "dependency", "cancelled", "deleting"] {
        let (db, board, queue, parent, _) = setup();
        TaskConversation::new(db.clone())
            .add_user_message_with_attachments_and_enqueue(
                &parent.id,
                Some("manager"),
                "Inspect existing work",
                &[],
            )
            .unwrap();
        match constraint {
            "backlog" => {
                db.conn()
                    .execute("UPDATE tasks SET backlog=1 WHERE id=?1", [&parent.id])
                    .unwrap();
            }
            "future" => {
                db.conn()
                    .execute(
                        "UPDATE tasks SET start_after='2999-01-01T00:00:00Z' WHERE id=?1",
                        [&parent.id],
                    )
                    .unwrap();
            }
            "dependency" => {
                let prerequisite = board
                    .create(&CreateTask {
                        title: "Prerequisite".into(),
                        agent_id: Some("peer".into()),
                        ..Default::default()
                    })
                    .unwrap();
                board.add_dependency(&parent.id, &prerequisite.id).unwrap();
            }
            "cancelled" => {
                board.update_status(&parent.id, "cancelled", None).unwrap();
            }
            _ => {
                db.conn().execute("UPDATE projects SET deletion_started_at=CURRENT_TIMESTAMP WHERE id='manager'", []).unwrap();
            }
        }
        assert!(queue.claim("manager").unwrap().is_none(), "{constraint}");
        assert!(queue.claim_next().unwrap().is_none(), "{constraint}");
    }
}

#[test]
fn wait_is_idempotent_and_a_later_response_from_the_same_parked_attempt_notifies_again() {
    let (db, board, queue, parent, child) = setup();
    queue.enqueue(&child.id, "worker").unwrap();
    let item = queue.claim("worker").unwrap().unwrap();
    let attempt = item.attempt_id.as_deref().unwrap();
    SessionManager::new(db.clone())
        .transition_attempt(
            attempt,
            "waiting_for_input",
            "Needs authorization",
            None,
            None,
        )
        .unwrap();
    board
        .update_status(&child.id, "waiting_for_input", None)
        .unwrap();
    let hold = request(&board, &parent, &child, attempt, ReviewDecision::Wait);
    review(db.clone(), "manager", &child.id, &hold).unwrap();
    let before: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM task_messages WHERE task_id=?1",
            [&child.id],
            |row| row.get(0),
        )
        .unwrap();
    review(db.clone(), "manager", &child.id, &hold).unwrap();
    let after: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM task_messages WHERE task_id=?1",
            [&child.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    SessionManager::new(db.clone())
        .transition_attempt(
            attempt,
            "completed",
            "Authorized and finished",
            Some("Deployment verified"),
            None,
        )
        .unwrap();
    queue.complete(item.id, "Deployment verified").unwrap();
    board.update_status(&child.id, "in_progress", None).unwrap();
    board.complete_and_roll_up(&child.id, None).unwrap();
    assert!(queue.claim("manager").unwrap().is_some());
    assert!(board.get(&child.id).unwrap().parent_review_required);
}

#[test]
fn revisions_continue_same_child_and_require_new_acceptance() {
    let (db, board, queue, parent, child) = setup();
    let old = finish_child(&db, &board, &queue, &child, "Not deployed yet.");
    review(
        db.clone(),
        "manager",
        &child.id,
        &request(&board, &parent, &child, &old, ReviewDecision::Revise),
    )
    .unwrap();
    let item = queue.claim("worker").unwrap().unwrap();
    assert_eq!(item.task_id, child.id);
    let next = item.attempt_id.unwrap();
    assert_ne!(old, next);
    assert!(review(
        db.clone(),
        "manager",
        &child.id,
        &request(&board, &parent, &child, &old, ReviewDecision::Accept)
    )
    .is_err());
    SessionManager::new(db.clone())
        .transition_attempt(
            &next,
            "completed",
            "Fixed",
            Some("Deployment complete"),
            None,
        )
        .unwrap();
    queue.complete(item.id, "Deployment complete").unwrap();
    board.complete_and_roll_up(&child.id, None).unwrap();
    assert!(board.get(&child.id).unwrap().parent_review_required);
    assert!(queue.claim("manager").unwrap().is_some());
    let (accepted, _) = review(
        db,
        "manager",
        &child.id,
        &request(&board, &parent, &child, &next, ReviewDecision::Accept),
    )
    .unwrap();
    assert_eq!(accepted.status, TaskStatus::Completed);
}
