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
    let (_, journal, mut command) = fixture();
    journal.admit("instance", &command, 200, 100).unwrap();
    journal
        .disable_binding("instance", &command.binding.id)
        .unwrap();
    command.binding.generation += 1;
    journal.bind("instance", &command.binding).unwrap();
    assert_eq!(journal.pending("instance").unwrap()[0].lease_until, 0);
    command.id = Uuid::new_v4().to_string();
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
