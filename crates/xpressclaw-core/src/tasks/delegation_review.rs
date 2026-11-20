//! Explicit acceptance of a worker's result by its delegating Agent.
use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::Deserialize;

use super::{
    board::{Task, TaskBoard},
    conversation::TaskConversation,
    delegation_notifications,
    queue::TaskQueue,
};
use crate::{
    db::Database,
    error::{Error, Result},
    projects::{agent_project, require_project_task},
};

#[cfg(test)]
#[path = "delegation_review_tests.rs"]
mod tests;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    pub parent_task_id: String,
    pub expected_attempt_id: String,
    pub expected_revision: i64,
    pub decision: ReviewDecision,
    pub guidance: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Accept,
    Revise,
    Wait,
}

impl ReviewDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Revise => "revise",
            Self::Wait => "wait",
        }
    }
}

pub fn review(
    db: Arc<Database>,
    caller: &str,
    child_id: &str,
    request: &ReviewRequest,
) -> Result<(Task, String)> {
    let guidance = request.guidance.as_deref().unwrap_or("").trim();
    if guidance.len() > 20_000
        || (request.decision != ReviewDecision::Accept && guidance.is_empty())
    {
        return Err(Error::Task(
            "Provide concrete guidance for revise/wait, up to 20000 bytes".into(),
        ));
    }
    let output = db.with_conn(|conn| {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let project = agent_project(&tx, caller)?;
        require_project_task(&tx, &project, &request.parent_task_id)?;
        require_project_task(&tx, &project, child_id)?;
        if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks parent JOIN tasks child ON child.parent_task_id=parent.id
             WHERE parent.id=?1 AND parent.agent_id=?2 AND child.id=?3
               AND parent.status NOT IN ('completed','cancelled'))",
            params![request.parent_task_id, caller, child_id], |row| row.get::<_, bool>(0),
        )? {
            return Err(Error::ProjectAccess("Only the active parent task's Agent can review this delegation".into()));
        }
        TaskQueue::ensure_locally_scheduled(&tx, child_id)?;
        TaskQueue::ensure_locally_scheduled(&tx, &request.parent_task_id)?;
        if tx.query_row("SELECT EXISTS(SELECT 1 FROM workflow_step_executions e JOIN workflow_instances i ON i.id=e.instance_id
            WHERE e.task_id=?1 AND e.task_id IS NOT i.source_task_id)", [child_id], |row| row.get::<_,bool>(0))? {
            return Err(Error::Task("This task is owned by a workflow; resolve it through its workflow".into()));
        }
        let latest: Option<(String, String, String)> = tx.query_row(
            "SELECT id,status,COALESCE(result,error_message,'') FROM work_attempts
             WHERE task_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1", [child_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        let (attempt, status, output) = latest.unwrap_or_default();
        if attempt != request.expected_attempt_id {
            return Err(Error::Task("The delegated task has a newer attempt; read its current result before reviewing".into()));
        }
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM task_queue WHERE task_id=?1 AND status IN ('queued','running'))
                 OR EXISTS(SELECT 1 FROM work_attempts WHERE task_id=?1
                    AND status IN ('queued','preparing','running','waiting_for_input','review'))",
            [child_id], |row| row.get(0),
        )?;
        if active && request.decision != ReviewDecision::Wait {
            return Err(Error::Task("The delegated task still has an active response; wait for it to finish before accepting or revising".into()));
        }
        delegation_notifications::reconcile_in_transaction(&tx)?;
        let notification: Option<i64> = tx.query_row(
            "SELECT id FROM task_delegation_notifications
             WHERE child_task_id=?1 AND child_attempt_id=?2 AND parent_task_id=?3 AND parent_agent_id=?4
             ORDER BY id DESC LIMIT 1",
            params![child_id, attempt, request.parent_task_id, caller], |row| row.get(0),
        ).optional()?;
        let notification = notification.ok_or_else(|| Error::Task("No finished delegated result is available to review yet".into()))?;
        let already_reviewed: bool = tx.query_row(
            "SELECT decision IS ?1 AND review_note IS ?2 FROM task_delegation_notifications WHERE id=?3",
            params![request.decision.as_str(),guidance,notification], |row| row.get(0),
        )?;
        if already_reviewed {
            tx.commit()?;
            return Ok(output);
        }
        let revision: i64 = tx.query_row("SELECT revision FROM tasks WHERE id=?1", [child_id], |row| row.get(0))?;
        if revision != request.expected_revision {
            return Err(Error::Task("The delegated task changed; read its current revision before reviewing".into()));
        }
        match request.decision {
            ReviewDecision::Accept => {
                if status != "completed" && !attempt.is_empty() {
                    return Err(Error::Task("Only a finished worker response can be accepted; request revisions or wait for the blocker instead".into()));
                }
                let blocked: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM tasks WHERE parent_task_id=?1 AND blocks_parent=1 AND status!='completed')
                         OR EXISTS(SELECT 1 FROM task_dependencies d JOIN tasks t ON t.id=d.depends_on_id WHERE d.task_id=?1 AND t.status!='completed')
                         OR EXISTS(SELECT 1 FROM task_pull_requests WHERE task_id=?1 AND status NOT IN ('approved','merged','cancelled'))
                         OR EXISTS(SELECT 1 FROM tasks WHERE id=?1 AND status='cancelled')",
                    [child_id], |row| row.get(0),
                )?;
                if blocked {
                    return Err(Error::Task("Required work and pull-request review must be resolved before accepting this result".into()));
                }
                TaskBoard::defer_reported_subtasks_on_conn(&tx, child_id, "parent_accepted", &chrono::Utc::now().to_rfc3339())?;
                tx.execute("UPDATE tasks SET parent_review_required=0,status='completed',
                    completed_at=COALESCE(completed_at,CURRENT_TIMESTAMP),updated_at=CURRENT_TIMESTAMP WHERE id=?1", [child_id])?;
            }
            ReviewDecision::Revise => {
                let agent: Option<String> = tx.query_row("SELECT agent_id FROM tasks WHERE id=?1", [child_id], |row| row.get(0))?;
                let agent = agent.ok_or_else(|| Error::Task("Assign an Agent before requesting revisions".into()))?;
                let message = TaskConversation::insert_text_message_in_transaction(&tx, child_id, "user",
                    &format!("Your parent task reviewed the result and requested revisions. Continue this same assignment.\n\n{guidance}"))?;
                TaskQueue::enqueue_continuation_for_message_in_transaction(&tx, child_id, &agent, message.id, &message.timestamp)?;
                tx.execute("UPDATE tasks SET parent_review_required=blocks_parent WHERE id=?1", [child_id])?;
            }
            ReviewDecision::Wait => {
                if !active {
                    tx.execute("UPDATE tasks SET parent_review_required=blocks_parent,status='blocked',completed_at=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?1", [child_id])?;
                }
                TaskConversation::insert_text_message_in_transaction(&tx, child_id, "user",
                    &format!("Your parent task reviewed the result. Keep this assignment open while the following blocker remains; do not treat it as completed.\n\n{guidance}"))?;
            }
        }
        tx.execute("UPDATE task_delegation_notifications SET decision=?1,review_note=?2,reviewed_at=CURRENT_TIMESTAMP WHERE id=?3",
            params![request.decision.as_str(),guidance,notification])?;
        tx.execute("INSERT INTO session_events(session_id,task_id,source_type,source_id,event_type,summary,payload)
            VALUES(?1,?2,'task',?3,'delegation_review',?4,?5)", params![caller,request.parent_task_id,child_id,
                format!("Parent review: {}",request.decision.as_str()),
                serde_json::json!({"task_id":child_id,"attempt_id":attempt,"decision":request.decision.as_str(),"guidance":guidance}).to_string()])?;
        delegation_notifications::retire_stale_in_transaction(&tx, Some(&request.parent_task_id))?;
        tx.commit()?;
        Ok::<_,Error>(output)
    })?;
    Ok((TaskBoard::new(db).get(child_id)?, output))
}
