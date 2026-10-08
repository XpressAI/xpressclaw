use super::*;
use crate::tasks::board::{CreateTask, TaskBoard};
use crate::tasks::queue::TaskQueue;

pub(super) struct LocalWork {
    pub conversation: Option<String>,
    pub turn: Option<String>,
    pub task: Option<String>,
    pub attempt: Option<String>,
}

pub(super) fn work_busy(
    conn: &rusqlite::Connection,
    instance: &str,
    command: &Command,
) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM connect_work w WHERE w.instance_id = ?1 AND w.binding_id = ?2 AND w.kind = ?3 AND w.work_id = ?4 AND (
            EXISTS(SELECT 1 FROM connect_commands c WHERE c.status = 'accepted' AND (c.conversation_id = w.local_id OR c.task_id = w.local_id))
            OR EXISTS(SELECT 1 FROM conversation_turns t WHERE t.conversation_id = w.local_id AND t.status IN ('queued','running'))
            OR EXISTS(SELECT 1 FROM task_queue q WHERE q.task_id = w.local_id AND q.status IN ('queued','running'))))",
        params![instance, command.binding.id, command.kind, command.work_id], |r| r.get(0),
    )?)
}

pub(super) fn admit_work(
    tx: &rusqlite::Transaction<'_>,
    instance: &str,
    command: &Command,
) -> Result<LocalWork> {
    if work_busy(tx, instance, command)? {
        return Err(invalid(
            "The platform work item already has an active execution",
        ));
    }
    let saved: Option<String> = tx.query_row(
        "SELECT local_id FROM connect_work WHERE instance_id = ?1 AND binding_id = ?2 AND kind = ?3 AND work_id = ?4",
        params![instance, command.binding.id, command.kind, command.work_id], |r| r.get(0),
    ).optional()?;
    let context = command.payload.history.join("\n");
    let content = format!(
        "Recent platform history:\n{context}\n\nCurrent request:\n{}",
        command.payload.text
    );
    let local_id = if let Some(id) = saved {
        id
    } else {
        let id = if command.kind == "task_turn" {
            TaskBoard::create_in_transaction(
                tx,
                &CreateTask {
                    title: format!(
                        "Xpress AI #{} · {}",
                        command.work_id,
                        command.payload.text.chars().take(120).collect::<String>()
                    ),
                    description: Some(command.payload.text.clone()),
                    agent_id: Some(command.binding.local_agent_id.clone()),
                    context: Some(
                        serde_json::json!({"origin": "xpress_ai_connect", "source_id": command.work_id, "project_id": command.binding.local_project_id, "session_mode": "new"}),
                    ),
                    ..Default::default()
                },
                None,
            )?
        } else {
            let id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO conversations (id, title, project_id) VALUES (?1, ?2, ?3)",
                params![
                    id,
                    format!("Xpress AI · chat {}", command.work_id),
                    command.binding.local_project_id
                ],
            )?;
            tx.execute("INSERT INTO conversation_participants (conversation_id, participant_type, participant_id) VALUES (?1, 'agent', ?2)", params![id, command.binding.local_agent_id])?;
            id
        };
        tx.execute("INSERT INTO connect_work (instance_id,binding_id,kind,work_id,local_id) VALUES (?1,?2,?3,?4,?5)",
            params![instance, command.binding.id, command.kind, command.work_id, id])?;
        id
    };
    if command.kind == "task_turn" {
        // A deleted or reassigned local projection must never acquire new authority.
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND agent_id=?2 AND project_id=?3)",
            params![
                local_id,
                command.binding.local_agent_id,
                command.binding.local_project_id
            ],
            |r| r.get(0),
        )?;
        if !valid {
            return Err(invalid("Connected task was deleted or reassigned"));
        }
        tx.execute("UPDATE tasks SET status='pending', completed_at=NULL, updated_at=CURRENT_TIMESTAMP WHERE id=?1", [&local_id])?;
        tx.execute(
            "INSERT INTO task_messages (task_id,role,content) VALUES (?1,'user',?2)",
            params![local_id, content],
        )?;
        let trigger = tx.last_insert_rowid();
        let queued =
            TaskQueue::enqueue_in_transaction(tx, &local_id, &command.binding.local_agent_id)?;
        tx.execute(
            "UPDATE work_attempts SET trigger_message_id=?2 WHERE id=?1",
            params![queued.attempt_id, trigger],
        )?;
        Ok(LocalWork {
            conversation: None,
            turn: None,
            task: Some(local_id),
            attempt: queued.attempt_id,
        })
    } else {
        tx.execute("INSERT INTO conversation_messages (conversation_id,sender_type,sender_id,sender_name,content) VALUES (?1,'user','xpress-ai','Xpress AI',?2)", params![local_id,content])?;
        let trigger = tx.last_insert_rowid();
        if !ConversationTurnQueue::enqueue_target_in_transaction(
            tx,
            &local_id,
            &command.binding.local_agent_id,
            trigger,
        )? {
            return Err(invalid("Connected chat cannot accept another turn"));
        }
        let turn = tx.query_row(
            "SELECT id FROM conversation_turns WHERE conversation_id=?1 AND trigger_message_id=?2",
            params![local_id, trigger],
            |r| r.get(0),
        )?;
        Ok(LocalWork {
            conversation: Some(local_id),
            turn: Some(turn),
            task: None,
            attempt: None,
        })
    }
}

impl ConnectJournal {
    /// Defer a new command before accepting its remote execution lease. Chat
    /// coalescing and task queue deduplication must never merge two receipts.
    pub fn can_admit(&self, instance: &str, command: &Command) -> Result<bool> {
        self.db.with_conn(|conn| {
            let replay: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM connect_commands WHERE id=?1)",
                [&command.id],
                |r| r.get(0),
            )?;
            Ok(replay || command.cancel_requested || !work_busy(conn, instance, command)?)
        })
    }

    pub fn command_for_execution(&self, execution: &str) -> Result<Option<String>> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT id FROM connect_commands WHERE turn_id=?1 OR attempt_id=?1",
                    [execution],
                    |r| r.get(0),
                )
                .optional()?)
        })
    }

    pub fn tool_execution_command(
        &self,
        instance: &str,
        command: &str,
        agent: &str,
        now: i64,
    ) -> Result<String> {
        self.db.with_conn(|conn| conn.query_row(
            "SELECT c.id FROM connect_commands c JOIN connect_bindings b ON b.id=c.binding_id JOIN agents a ON a.id=b.local_agent_id AND a.project_id=b.local_project_id JOIN projects p ON p.id=b.local_project_id AND p.deletion_started_at IS NULL
             WHERE c.id=?1 AND c.instance_id=?2 AND b.local_agent_id=?3 AND b.active=1 AND b.generation=c.binding_generation AND c.status='accepted' AND c.lease_until>?4
             AND (EXISTS(SELECT 1 FROM conversation_turns t WHERE t.id=c.turn_id AND t.status='running') OR EXISTS(SELECT 1 FROM work_attempts a JOIN tasks t ON t.id=a.task_id WHERE a.id=c.attempt_id AND a.status='running' AND t.agent_id=b.local_agent_id AND t.project_id=b.local_project_id))",
            params![command,instance,agent,now], |r|r.get(0)).optional()?.ok_or_else(||invalid("No authorized connected execution for this Agent")))
    }

    /// Stop the journal's exact attempt, rechecking the lease under the writer
    /// lock so a concurrent heartbeat cannot lose renewed authorization.
    pub fn stop_task(&self, command: &str, expired_at: Option<i64>) -> Result<Option<String>> {
        let stopped = self.db.with_conn(|conn| -> Result<Option<String>> {
            let tx=conn.unchecked_transaction()?;
            let execution: Option<(String,String,String)> = tx.query_row(
                "SELECT c.task_id,c.attempt_id,a.status FROM connect_commands c JOIN work_attempts a ON a.id=c.attempt_id
                 WHERE c.id=?1 AND c.status='accepted' AND (?2 IS NULL OR c.lease_until<=?2)
                   AND a.status NOT IN ('completed','failed','cancelled','interrupted')",
                params![command,expired_at], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((task,attempt,status))=execution else { return Ok(None); };
            let uncertain=expired_at.is_some() && status!="queued";
            let error=if uncertain { "execution_outcome_unknown_after_lease_expiry" } else { "execution_cancelled" };
            tx.execute("UPDATE work_attempts SET status=?2,error_message=?3,native_session_id=NULL,completed_at=CURRENT_TIMESTAMP WHERE id=?1", params![attempt,if uncertain {"failed"}else{"cancelled"},error])?;
            // Retain running queue/container ownership until the worker exits.
            tx.execute("UPDATE task_queue SET status='failed',harness_response=?2,completed_at=CURRENT_TIMESTAMP WHERE attempt_id=?1 AND status='queued'", params![attempt,error])?;
            tx.execute("UPDATE tasks SET status=?2,active_attempt_id=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?1", params![task,if uncertain {"blocked"}else{"cancelled"}])?;
            tx.commit()?;
            Ok(Some(attempt))
        })?;
        if let Some(id) = &stopped {
            let sessions = crate::sessions::SessionManager::new(self.db.clone());
            let attempt = sessions.get_attempt(id)?;
            sessions.refresh_status(&attempt.session_id)?;
        }
        Ok(stopped)
    }
}
