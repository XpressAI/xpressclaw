use super::*;
use crate::agents::registry::AgentRegistry;
use crate::conversations::{ConversationManager, SendMessage};

fn fixture() -> (Arc<Database>, ConnectJournal, Command) {
    let db = Arc::new(Database::open_memory().unwrap());
    AgentRegistry::new(db.clone())
        .ensure("atlas", "native")
        .unwrap();
    let journal = ConnectJournal::new(db.clone());
    let binding = Binding {
        id: Uuid::new_v4().to_string(),
        local_project_id: "atlas".into(),
        local_agent_id: "atlas".into(),
        project_id: "cloud-project".into(),
        agent_name: "local-atlas".into(),
        generation: 1,
        active: true,
    };
    journal.bind("instance", &binding).unwrap();
    let command = Command {
        version: 1,
        id: Uuid::new_v4().to_string(),
        binding,
        kind: "chat_turn".into(),
        work_id: Uuid::new_v4().to_string(),
        source_conversation_id: None,
        payload: TurnPayload {
            text: "Review this change".into(),
            history: vec![],
        },
        expires_at: "2030-01-01T00:00:00Z".into(),
        cancel_requested: false,
    };
    (db, journal, command)
}

#[test]
fn agents_in_one_local_project_must_share_the_platform_project() {
    let (db, journal, command) = fixture();
    AgentRegistry::new(db.clone())
        .ensure("second", "native")
        .unwrap();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE agents SET project_id = 'atlas' WHERE id = 'second'",
            [],
        )
    })
    .unwrap();
    let mut second = command.binding.clone();
    second.id = Uuid::new_v4().to_string();
    second.local_agent_id = "second".into();
    second.agent_name = "local-second".into();
    second.project_id = "other-team".into();
    assert!(journal
        .validate_project_mapping("instance", &second.local_project_id, &second.project_id)
        .is_err());
    assert!(journal.bind("instance", &second).is_err());
    assert_eq!(journal.bindings("instance").unwrap().len(), 1);
    journal
        .disable_binding("instance", &command.binding.id)
        .unwrap();
    assert!(journal.bind("instance", &second).is_err());
    second.project_id = command.binding.project_id.clone();
    journal.bind("instance", &second).unwrap();
    assert_eq!(journal.bindings("instance").unwrap().len(), 2);
}

#[test]
fn separate_local_projects_and_new_pairings_can_choose_their_own_mapping() {
    let (db, journal, command) = fixture();
    AgentRegistry::new(db.clone())
        .ensure("second", "native")
        .unwrap();
    let mut second = command.binding.clone();
    second.id = Uuid::new_v4().to_string();
    second.local_project_id = "second".into();
    second.local_agent_id = "second".into();
    second.project_id = "other-team".into();
    second.agent_name = "local-second".into();
    journal.bind("instance", &second).unwrap();
    let mut paired = command.binding.clone();
    paired.id = Uuid::new_v4().to_string();
    paired.project_id = "new-team".into();
    journal.bind("new-instance", &paired).unwrap();
}

#[test]
fn replay_after_completion_does_not_queue_another_turn() {
    let (db, journal, command) = fixture();
    let first = journal.admit("instance", &command, 200, 100).unwrap();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE conversation_turns SET status = 'completed' WHERE id = ?1",
            [first.turn_id.as_ref().unwrap()],
        )
    })
    .unwrap();
    journal.collect("instance").unwrap();
    journal.acknowledge("instance", &command.id).unwrap();
    let replay = journal.admit("instance", &command, 300, 200).unwrap();
    assert_eq!(replay.turn_id, first.turn_id);
    assert_eq!(replay.receipt.status, "completed");
    assert!(replay.acknowledged);
    let count: i64 = db
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM conversation_turns", [], |row| {
                row.get(0)
            })
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn conflicting_payload_or_instance_cannot_reuse_a_command_id() {
    let (_, journal, mut command) = fixture();
    journal.admit("instance", &command, 200, 100).unwrap();
    assert!(journal.admit("other-instance", &command, 200, 100).is_err());
    command.payload.text.push_str(" changed");
    assert!(journal.admit("instance", &command, 200, 100).is_err());
}

#[test]
fn unapproved_agent_project_and_generation_are_rejected_without_work() {
    let (db, journal, command) = fixture();
    for field in ["agent", "project", "generation"] {
        let mut changed = command.clone();
        match field {
            "agent" => changed.binding.local_agent_id = "someone-else".into(),
            "project" => changed.binding.project_id = "other-team".into(),
            _ => changed.binding.generation += 1,
        }
        assert!(journal.admit("instance", &changed, 200, 100).is_err());
    }
    let count: i64 = db
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn cancellation_before_delivery_is_a_durable_tombstone() {
    let (db, journal, mut command) = fixture();
    command.cancel_requested = true;
    let first = journal.admit("instance", &command, 0, 100).unwrap();
    assert_eq!(first.receipt.status, "cancelled");
    assert!(first.turn_id.is_none());
    command.cancel_requested = false;
    assert_eq!(
        journal
            .admit("instance", &command, 200, 100)
            .unwrap()
            .receipt
            .status,
        "cancelled"
    );
    let count: i64 = db
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM conversation_turns", [], |row| {
                row.get(0)
            })
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn expired_commands_never_enter_the_runner_queue() {
    let (_, journal, mut command) = fixture();
    assert!(journal.admit("instance", &command, 100, 100).is_err());
    command.expires_at = "1970-01-01T00:00:01Z".into();
    let execution = journal.admit("instance", &command, 200, 100).unwrap();
    assert_eq!(execution.receipt.status, "cancelled");
    assert!(execution.turn_id.is_none());
}

#[test]
fn disabling_binding_expires_active_work_and_blocks_new_work() {
    let (_, journal, command) = fixture();
    journal.admit("instance", &command, 200, 100).unwrap();
    journal
        .disable_binding("instance", &command.binding.id)
        .unwrap();
    assert_eq!(journal.pending("instance").unwrap()[0].lease_until, 0);
    let mut next = command.clone();
    next.id = Uuid::new_v4().to_string();
    assert!(journal.admit("instance", &next, 200, 100).is_err());
}

#[test]
fn imported_messages_do_not_start_a_second_local_response() {
    let (db, journal, command) = fixture();
    let execution = journal.admit("instance", &command, 200, 100).unwrap();
    let conversation = execution.conversation_id.unwrap();
    let message = ConversationManager::new(db.clone())
        .send_message(
            &conversation,
            &SendMessage {
                sender_type: "user".into(),
                sender_id: "local".into(),
                sender_name: None,
                content: "Another request".into(),
                message_type: None,
            },
        )
        .unwrap();
    let targets = ConversationTurnQueue::new(db)
        .enqueue_for_message(&conversation, message.id, "user", "local", &message.content)
        .unwrap();
    assert!(targets.is_empty());
}

#[test]
fn restart_reports_unknown_outcome_instead_of_reexecuting() {
    let (db, journal, command) = fixture();
    let execution = journal.admit("instance", &command, 200, 100).unwrap();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE conversation_turns SET status = 'running' WHERE id = ?1",
            [execution.turn_id.as_ref().unwrap()],
        )
    })
    .unwrap();
    journal.recover().unwrap();
    ConversationTurnQueue::new(db.clone()).recover().unwrap();
    journal.collect("instance").unwrap();
    let replay = journal.admit("instance", &command, 300, 200).unwrap();
    assert_eq!(replay.receipt.status, "failed");
    assert_eq!(
        replay.receipt.error.as_deref(),
        Some("execution_state_lost_after_restart")
    );
    assert!(ConversationTurnQueue::new(db)
        .claim_next()
        .unwrap()
        .is_none());
}

#[test]
fn disabled_binding_cannot_be_revived_by_an_inflight_heartbeat() {
    let (_, journal, command) = fixture();
    journal.admit("instance", &command, 200, 100).unwrap();
    journal
        .disable_binding("instance", &command.binding.id)
        .unwrap();
    journal.renew("instance", &command.id, 300).unwrap();
    assert_eq!(journal.pending("instance").unwrap()[0].lease_until, 0);
}

#[test]
fn cancellation_replay_can_change_remote_active_flag_without_reexecution() {
    let (_, journal, mut command) = fixture();
    let first = journal.admit("instance", &command, 200, 100).unwrap();
    command.binding.active = false;
    command.cancel_requested = true;
    let repeated = journal.admit("instance", &command, 0, 110).unwrap();
    assert_eq!(first.turn_id, repeated.turn_id);
}

#[test]
fn connected_tools_require_the_owning_instance_agent_and_live_lease() {
    let (_, journal, command) = fixture();
    let turn = journal.admit("instance", &command, 200, 100).unwrap();
    let conversation = turn.conversation_id.unwrap();
    assert_eq!(
        journal
            .tool_command("instance", &conversation, "atlas", 110)
            .unwrap(),
        command.id
    );
    assert!(journal
        .tool_command("other", &conversation, "atlas", 110)
        .is_err());
    assert!(journal
        .tool_command("instance", &conversation, "other", 110)
        .is_err());
    assert!(journal
        .tool_command("instance", &conversation, "atlas", 200)
        .is_err());
    journal
        .disable_binding("instance", &command.binding.id)
        .unwrap();
    assert!(journal
        .tool_command("instance", &conversation, "atlas", 110)
        .is_err());
}

#[test]
fn reactivating_binding_advances_generation_and_expires_old_work() {
    let (db, journal, mut command) = fixture();
    let old = journal.admit("instance", &command, 200, 100).unwrap();
    journal
        .disable_binding("instance", &command.binding.id)
        .unwrap();
    command.binding.generation += 1;
    journal.bind("instance", &command.binding).unwrap();
    assert_eq!(journal.pending("instance").unwrap()[0].lease_until, 0);
    command.id = Uuid::new_v4().to_string();
    assert!(!journal.can_admit("instance", &command).unwrap());
    ConversationTurnQueue::new(db)
        .expire_lease(
            old.conversation_id.as_deref().unwrap(),
            old.turn_id.as_deref().unwrap(),
            &old.id,
            210,
        )
        .unwrap();
    journal.collect("instance").unwrap();
    assert!(journal.admit("instance", &command, 300, 210).is_ok());
}

#[test]
fn moved_agents_lose_callback_authority_and_deleted_turns_report_unknown_outcome() {
    let (db, journal, command) = fixture();
    let execution = journal.admit("instance", &command, 200, 100).unwrap();
    db.with_conn(|conn| conn.execute("UPDATE agents SET project_id = NULL WHERE id = 'atlas'", []))
        .unwrap();
    assert!(journal
        .tool_command(
            "instance",
            execution.conversation_id.as_ref().unwrap(),
            "atlas",
            110
        )
        .is_err());
    journal.renew("instance", &command.id, 300).unwrap();
    assert_eq!(journal.pending("instance").unwrap()[0].lease_until, 200);
    db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM conversation_turns WHERE id = ?1",
            [execution.turn_id.unwrap()],
        )
    })
    .unwrap();
    journal.collect("instance").unwrap();
    let result = journal.pending("instance").unwrap();
    assert_eq!(result[0].receipt.status, "failed");
    assert_eq!(
        result[0].receipt.error.as_deref(),
        Some("execution_state_lost_after_restart")
    );
}

#[test]
fn expired_running_lease_records_unknown_outcome_and_cannot_be_replayed() {
    let (db, journal, command) = fixture();
    let execution = journal.admit("instance", &command, 200, 100).unwrap();
    let queue = ConversationTurnQueue::new(db.clone());
    let running = queue.claim_next().unwrap().unwrap();
    db.with_conn(|conn| conn.execute(
        "UPDATE conversation_agent_sessions SET native_session_id = 'old-session' WHERE conversation_id = ?1",
        [&running.conversation_id],
    )).unwrap();
    let stopped = queue
        .expire_lease(
            execution.conversation_id.as_ref().unwrap(),
            &running.id,
            &command.id,
            200,
        )
        .unwrap();
    assert!(stopped.was_running);
    assert_eq!(stopped.turn.status, "failed");
    assert!(
        !queue
            .expire_lease(&running.conversation_id, &running.id, &command.id, 200)
            .unwrap()
            .changed
    );
    let native: Option<String> = db
        .with_conn(|conn| {
            conn.query_row(
        "SELECT native_session_id FROM conversation_agent_sessions WHERE conversation_id = ?1",
        [&running.conversation_id], |row| row.get(0),
    )
        })
        .unwrap();
    assert!(native.is_none());
    journal.collect("instance").unwrap();
    let replay = journal.admit("instance", &command, 400, 300).unwrap();
    assert_eq!(replay.receipt.status, "failed");
    assert_eq!(
        replay.receipt.error.as_deref(),
        Some("execution_outcome_unknown_after_lease_expiry")
    );
    assert!(queue.claim_next().unwrap().is_none());
}

#[test]
fn expired_queued_lease_can_be_cancelled_without_execution() {
    let (db, journal, command) = fixture();
    let execution = journal.admit("instance", &command, 200, 100).unwrap();
    let queue = ConversationTurnQueue::new(db);
    let stopped = queue
        .expire_lease(
            execution.conversation_id.as_ref().unwrap(),
            execution.turn_id.as_ref().unwrap(),
            &command.id,
            200,
        )
        .unwrap();
    assert!(!stopped.was_running);
    journal.collect("instance").unwrap();
    assert_eq!(
        journal.pending("instance").unwrap()[0].receipt.status,
        "cancelled"
    );
    assert!(queue.claim_next().unwrap().is_none());
}

#[test]
fn renewed_lease_survives_a_stale_watchdog_snapshot() {
    let (db, journal, command) = fixture();
    journal.admit("instance", &command, 200, 100).unwrap();
    let queue = ConversationTurnQueue::new(db.clone());
    let running = queue.claim_next().unwrap().unwrap();
    let snapshot = journal.pending("instance").unwrap().remove(0);
    assert_eq!(snapshot.lease_until, 200);
    journal.renew("instance", &command.id, 400).unwrap();
    let stopped = queue
        .expire_lease(&running.conversation_id, &running.id, &snapshot.id, 200)
        .unwrap();
    assert!(!stopped.changed);
    assert_eq!(stopped.turn.status, "running");
    journal.collect("instance").unwrap();
    let pending = journal.pending("instance").unwrap().remove(0);
    assert_eq!(pending.receipt.status, "accepted");
    assert_eq!(pending.lease_until, 400);
    assert!(
        queue
            .expire_lease(&running.conversation_id, &running.id, &command.id, 400)
            .unwrap()
            .changed
    );
}

fn task_fixture() -> (Arc<Database>, ConnectJournal, Command, i64) {
    let (db, journal, mut command) = fixture();
    command.kind = "task_turn".into();
    command.work_id = "38168".into();
    (db, journal, command, chrono::Utc::now().timestamp())
}

#[test]
fn connected_task_uses_native_queue_and_replays_without_conversations() {
    let (db, journal, command, now) = task_fixture();
    let first = journal.admit("instance", &command, now + 90, now).unwrap();
    assert!(first.conversation_id.is_none());
    assert!(first.turn_id.is_none());
    let task = first.task_id.as_deref().unwrap();
    let queue = crate::tasks::queue::TaskQueue::new(db.clone());
    assert!(queue.enqueue(task, "atlas").is_err());
    assert!(queue.enqueue_continuation(task, "atlas").is_err());
    let item = queue.claim("atlas").unwrap().unwrap();
    assert_eq!(item.task_id, task);
    assert_eq!(item.attempt_id, first.attempt_id);
    let replay = journal.admit("instance", &command, now + 90, now).unwrap();
    assert_eq!(replay.attempt_id, first.attempt_id);
    assert_eq!(replay.task_id, first.task_id);
    db.with_conn(|conn| {
        for table in ["tasks", "task_queue", "work_attempts", "task_messages"] {
            let count: i64 =
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
            assert_eq!(count, 1, "{table}");
        }
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        Ok::<_, Error>(())
    })
    .unwrap();
}

#[test]
fn subsequent_platform_attempt_reuses_task_and_returns_full_result() {
    let (db, journal, mut command, now) = task_fixture();
    let first = journal.admit("instance", &command, now + 90, now).unwrap();
    let queue = crate::tasks::queue::TaskQueue::new(db.clone());
    let item = queue.claim("atlas").unwrap().unwrap();
    let output = "finished ".repeat(1000);
    crate::sessions::SessionManager::new(db.clone())
        .transition_attempt(
            first.attempt_id.as_deref().unwrap(),
            "completed",
            "done",
            Some(&output),
            None,
        )
        .unwrap();
    queue.complete(item.id, &output).unwrap();
    journal.collect("instance").unwrap();
    journal.acknowledge("instance", &command.id).unwrap();
    assert_eq!(
        journal
            .admit("instance", &command, now + 90, now)
            .unwrap()
            .receipt
            .text,
        Some(output)
    );
    command.id = Uuid::new_v4().to_string();
    command.payload.text = "Continue the existing assignment".into();
    assert!(journal.can_admit("instance", &command).unwrap());
    let second = journal.admit("instance", &command, now + 90, now).unwrap();
    assert_eq!(first.task_id, second.task_id);
    assert_ne!(first.attempt_id, second.attempt_id);
    assert_eq!(
        crate::tasks::board::TaskBoard::new(db.clone())
            .list_all(None, None, 100)
            .unwrap()
            .len(),
        1
    );
    assert!(queue.claim("atlas").unwrap().is_some());
}

#[test]
fn chat_commands_reuse_conversation_but_never_merge_receipts() {
    let (db, journal, mut command) = fixture();
    let first = journal.admit("instance", &command, 200, 100).unwrap();
    command.id = Uuid::new_v4().to_string();
    assert!(!journal.can_admit("instance", &command).unwrap());
    assert!(journal.admit("instance", &command, 200, 100).is_err());
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE conversation_turns SET status='completed' WHERE id=?1",
            [first.turn_id.as_deref().unwrap()],
        )
    })
    .unwrap();
    journal.collect("instance").unwrap();
    assert!(journal.can_admit("instance", &command).unwrap());
    let second = journal.admit("instance", &command, 200, 100).unwrap();
    assert_eq!(first.conversation_id, second.conversation_id);
    assert_ne!(first.turn_id, second.turn_id);
    assert_eq!(journal.pending("instance").unwrap().len(), 2);
}

#[test]
fn connected_task_callbacks_and_claims_require_the_live_command_lease() {
    let (db, journal, command, now) = task_fixture();
    let first = journal.admit("instance", &command, now + 90, now).unwrap();
    let attempt = first.attempt_id.as_deref().unwrap();
    assert!(journal
        .tool_execution_command("instance", &command.id, "atlas", now)
        .is_err());
    assert!(journal
        .tool_execution_command("other", &command.id, "atlas", now)
        .is_err());
    let queue = crate::tasks::queue::TaskQueue::new(db.clone());
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE tasks SET agent_id=NULL WHERE id=?1",
            [first.task_id.as_deref().unwrap()],
        )
    })
    .unwrap();
    assert!(queue.claim("atlas").unwrap().is_none());
    journal.renew("instance", &command.id, now + 180).unwrap();
    assert_eq!(
        journal.pending("instance").unwrap()[0].lease_until,
        now + 90
    );
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE tasks SET agent_id='atlas' WHERE id=?1",
            [first.task_id.as_deref().unwrap()],
        )
    })
    .unwrap();
    assert!(queue
        .claim_at(None, chrono::DateTime::from_timestamp(now + 91, 0).unwrap())
        .unwrap()
        .is_none());
    queue
        .claim_at(None, chrono::DateTime::from_timestamp(now, 0).unwrap())
        .unwrap()
        .unwrap();
    crate::sessions::SessionManager::new(db.clone())
        .transition_attempt(attempt, "running", "working", None, None)
        .unwrap();
    assert_eq!(
        journal
            .tool_execution_command("instance", &command.id, "atlas", now)
            .unwrap(),
        command.id
    );
    assert!(journal
        .tool_execution_command("instance", &command.id, "other", now)
        .is_err());
    assert!(journal
        .tool_execution_command("instance", &command.id, "atlas", now + 91)
        .is_err());
    journal.renew("instance", &command.id, now + 200).unwrap();
    assert!(journal
        .stop_task(&command.id, Some(now + 91))
        .unwrap()
        .is_none());
    assert!(journal
        .stop_task(&command.id, Some(now + 201))
        .unwrap()
        .is_some());
    journal.collect("instance").unwrap();
    let result = journal.pending("instance").unwrap().remove(0);
    assert_eq!(result.receipt.status, "failed");
    assert_eq!(
        result.receipt.error.as_deref(),
        Some("execution_outcome_unknown_after_lease_expiry")
    );
    assert!(journal
        .tool_execution_command("instance", &command.id, "atlas", now)
        .is_err());
    assert!(journal.stop_task(&command.id, None).unwrap().is_none());
}

#[test]
fn cancelling_queued_connected_task_and_recovery_cannot_restart_it() {
    let (db, journal, mut command, now) = task_fixture();
    let first = journal.admit("instance", &command, now + 90, now).unwrap();
    journal.stop_task(&command.id, None).unwrap();
    journal.collect("instance").unwrap();
    assert_eq!(
        journal.pending("instance").unwrap()[0].receipt.status,
        "cancelled"
    );
    let queue = crate::tasks::queue::TaskQueue::new(db.clone());
    assert!(queue.claim("atlas").unwrap().is_none());
    command.id = Uuid::new_v4().to_string();
    let second = journal.admit("instance", &command, now + 90, now).unwrap();
    assert_eq!(first.task_id, second.task_id);
    journal.recover().unwrap();
    queue.recover_in_progress().unwrap();
    journal.collect("instance").unwrap();
    assert!(queue.claim("atlas").unwrap().is_none());
    let execution = journal
        .pending("instance")
        .unwrap()
        .into_iter()
        .find(|r| r.id == second.id)
        .unwrap();
    assert_eq!(
        execution.receipt.error.as_deref(),
        Some("execution_state_lost_after_restart")
    );
}

#[test]
fn connected_tasks_never_attach_default_workflows() {
    use crate::tasks::board::{CreateTask, TaskBoard};
    use crate::tasks::queue::TaskQueue;
    use crate::workflows::engine::WorkflowEngine;
    use crate::workflows::instance::InstanceManager;
    use crate::workflows::manager::{CreateWorkflow, WorkflowManager};

    let (db, journal, command, now) = task_fixture();
    let workflows = WorkflowManager::new(db.clone());
    let workflow = workflows.create(&CreateWorkflow {
        name: "local-follow-up".into(),
        description: None,
        yaml_content: "name: local-follow-up\nflows:\n  main:\n    steps:\n      - id: follow_up\n        type: continue\n        prompt: Do a local follow-up\n".into(),
    }).unwrap();
    workflows.set_default_for_tasks(&workflow.id, true).unwrap();
    let engine = WorkflowEngine::new(db.clone());
    let execution = journal.admit("instance", &command, now + 90, now).unwrap();
    let task = execution.task_id.as_deref().unwrap();
    let queue = TaskQueue::new(db.clone());
    let claimed = queue.claim("atlas").unwrap().unwrap();
    assert!(engine
        .attach_default_workflows_to_task(task)
        .unwrap()
        .is_empty());
    crate::sessions::SessionManager::new(db.clone())
        .transition_attempt(
            execution.attempt_id.as_deref().unwrap(),
            "completed",
            "Done",
            Some("Done"),
            None,
        )
        .unwrap();
    queue.complete(claimed.id, "Done").unwrap();
    engine.on_task_completed(task, "completed", "Done").unwrap();
    assert!(InstanceManager::new(db.clone())
        .list_instances(&workflow.id, 10)
        .unwrap()
        .is_empty());
    assert!(queue.claim("atlas").unwrap().is_none());
    journal.collect("instance").unwrap();
    assert_eq!(
        journal.pending("instance").unwrap()[0].receipt.status,
        "completed"
    );

    // The same default still attaches to ordinary work in this project.
    let local = TaskBoard::new(db.clone())
        .create(&CreateTask {
            title: "Local work".into(),
            agent_id: Some("atlas".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        engine
            .attach_default_workflows_to_task(&local.id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn local_messages_cannot_coalesce_interrupt_or_strand_connected_tasks() {
    use crate::sessions::SessionManager;
    use crate::tasks::attachments::DecodedImageAttachment;
    use crate::tasks::conversation::TaskConversation;
    use crate::tasks::queue::TaskQueue;

    for status in ["queued", "running", "completed"] {
        let (db, journal, mut command, now) = task_fixture();
        let execution = journal.admit("instance", &command, now + 90, now).unwrap();
        let task = execution.task_id.as_deref().unwrap();
        let attempt = execution.attempt_id.as_deref().unwrap();
        let sessions = SessionManager::new(db.clone());
        let queue = TaskQueue::new(db.clone());
        if status != "queued" {
            let claimed = queue.claim("atlas").unwrap().unwrap();
            sessions
                .transition_attempt(attempt, status, "Test", Some("Done"), None)
                .unwrap();
            if status == "completed" {
                queue.complete(claimed.id, "Done").unwrap();
                journal.collect("instance").unwrap();
            }
        }
        let messages = TaskConversation::new(db.clone());
        let original = messages.get_messages(task).unwrap().remove(0);
        for agent in [Some("atlas"), None] {
            let error = messages
                .add_user_message_with_attachments_and_enqueue(
                    task,
                    agent,
                    "Local interruption",
                    &[DecodedImageAttachment {
                        name: "local.txt".into(),
                        mime_type: "text/plain".into(),
                        data: b"Local file".to_vec(),
                    }],
                )
                .unwrap_err();
            assert!(
                error.to_string().contains("continue it on the platform"),
                "{status}: {error}"
            );
        }
        // The queue API must also guard callers that already own a message.
        assert!(queue
            .enqueue_continuation_for_message(task, "atlas", original.id, &original.timestamp)
            .is_err());
        assert!(queue
            .enqueue_review_follow_up_for_current_agent(task, "Local review")
            .is_err());
        assert_eq!(messages.get_messages(task).unwrap().len(), 1, "{status}");
        assert_eq!(sessions.get_attempt(attempt).unwrap().status, status);
        db.with_conn(|conn| {
            for table in ["task_queue", "work_attempts"] {
                let count: i64 =
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
                assert_eq!(count, 1, "{status}: {table}");
            }
            let attachments: i64 =
                conn.query_row("SELECT COUNT(*) FROM task_message_attachments", [], |r| {
                    r.get(0)
                })?;
            assert_eq!(attachments, 0);
            let trigger: i64 = conn.query_row(
                "SELECT trigger_message_id FROM work_attempts WHERE id=?1",
                [attempt],
                |r| r.get(0),
            )?;
            assert_eq!(trigger, original.id);
            Ok::<_, Error>(())
        })
        .unwrap();
        command.id = Uuid::new_v4().to_string();
        assert_eq!(
            journal.can_admit("instance", &command).unwrap(),
            status == "completed"
        );
        if status == "completed" {
            assert_eq!(
                journal
                    .admit("instance", &command, now + 90, now)
                    .unwrap()
                    .task_id,
                execution.task_id
            );
        }
    }
}

#[test]
fn planner_and_transactional_enqueue_cannot_modify_connected_tasks() {
    use crate::tasks::planning::{PlanningAction, PlanningChange, PlanningFilter, TaskPlanner};
    use crate::tasks::queue::TaskQueue;

    let (db, journal, command, now) = task_fixture();
    AgentRegistry::new(db.clone())
        .ensure("helper", "native")
        .unwrap();
    db.with_conn(|conn| conn.execute("UPDATE agents SET project_id='atlas' WHERE id='helper'", []))
        .unwrap();
    let execution = journal.admit("instance", &command, now + 90, now).unwrap();
    let task = execution.task_id.as_deref().unwrap();
    let planner = TaskPlanner::new(db.clone());
    let before = planner.get(task).unwrap();
    assert!(before
        .planning
        .disabled_reason
        .as_deref()
        .unwrap()
        .contains("scheduled by Xpress AI"));
    assert_eq!(
        planner.list(&PlanningFilter::default()).unwrap().tasks[0]
            .planning
            .disabled_reason,
        before.planning.disabled_reason
    );
    let snapshot = serde_json::to_value(&before).unwrap();
    for change in [
        PlanningAction::Schedule {
            start_after: Some("2099-01-01T00:00:00Z".into()),
            activate: true,
        },
        PlanningAction::Backlog,
        PlanningAction::Queue,
        PlanningAction::Assign {
            agent_id: "helper".into(),
        },
        PlanningAction::Priority { priority: 100 },
        PlanningAction::Reorder {
            before_id: Some("other".into()),
            after_id: None,
        },
    ] {
        let error = planner
            .change(
                task,
                &PlanningChange {
                    expected_revision: before.task.revision,
                    change,
                },
            )
            .err()
            .unwrap();
        assert!(error.to_string().contains("continue it on the platform"));
        assert_eq!(
            serde_json::to_value(planner.get(task).unwrap()).unwrap(),
            snapshot
        );
    }
    db.with_conn(|conn| {
        let tx = conn.unchecked_transaction().unwrap();
        assert!(TaskQueue::enqueue_in_transaction(&tx, task, "helper").is_err());
    });
    // Neither the assignment nor the platform command's attempt was changed.
    let queue = TaskQueue::new(db.clone());
    assert!(queue.claim("helper").unwrap().is_none());
    let claimed = queue.claim("atlas").unwrap().unwrap();
    assert_eq!(claimed.attempt_id, execution.attempt_id);
    assert_eq!(
        crate::sessions::SessionManager::new(db)
            .get_attempt(claimed.attempt_id.as_deref().unwrap())
            .unwrap()
            .session_id,
        "atlas"
    );
}

#[test]
fn cancelled_or_expired_claimed_task_accepts_next_command_after_worker_exit() {
    use crate::sessions::SessionManager;
    use crate::tasks::queue::TaskQueue;

    for phase in ["queued", "preparing", "running"] {
        for expire in [false, true] {
            let (db, journal, mut command, now) = task_fixture();
            let execution = journal.admit("instance", &command, now + 90, now).unwrap();
            let attempt = execution.attempt_id.as_deref().unwrap();
            let queue = TaskQueue::new(db.clone());
            let claimed = queue.claim("atlas").unwrap().unwrap();
            let sessions = SessionManager::new(db.clone());
            if phase != "queued" {
                sessions
                    .transition_attempt(attempt, phase, "Test", None, None)
                    .unwrap();
            }
            if phase == "running" {
                sessions
                    .set_container(attempt, "running-container")
                    .unwrap();
            }
            journal
                .stop_task(&command.id, expire.then_some(now + 91))
                .unwrap();
            journal.collect("instance").unwrap();
            assert_eq!(queue.get(claimed.id).unwrap().status, "running");
            command.id = Uuid::new_v4().to_string();
            assert!(!journal.can_admit("instance", &command).unwrap());
            if phase == "running" {
                assert_eq!(
                    queue
                        .finalize_released_terminal_dispatch(claimed.id)
                        .unwrap()
                        .status,
                    "running"
                );
                sessions.clear_container(attempt).unwrap();
            }
            assert_eq!(
                queue
                    .finalize_released_terminal_dispatch(claimed.id)
                    .unwrap()
                    .status,
                "failed"
            );
            assert!(journal.can_admit("instance", &command).unwrap());
            let next = journal
                .admit("instance", &command, now + 200, now + 92)
                .unwrap();
            assert_eq!(next.task_id, execution.task_id);
            assert_eq!(
                queue.claim("atlas").unwrap().unwrap().attempt_id,
                next.attempt_id
            );
        }
    }
}
