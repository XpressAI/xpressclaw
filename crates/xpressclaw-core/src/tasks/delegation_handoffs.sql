ALTER TABLE tasks ADD COLUMN parent_review_required INTEGER NOT NULL DEFAULT 0;

-- Preserve completed historical work. Active delegations join the review
-- protocol on upgrade; every subsequent dispatched turn renews this gate.
UPDATE tasks SET parent_review_required = 1
WHERE parent_task_id IS NOT NULL AND blocks_parent = 1
  AND status NOT IN ('completed', 'cancelled') AND hidden = 0 AND provenance != 'native_plan'
  AND agent_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM tasks parent JOIN agents owner ON owner.id = parent.agent_id
              WHERE parent.id = tasks.parent_task_id AND parent.project_id = tasks.project_id
                AND owner.project_id = parent.project_id AND parent.status NOT IN ('completed', 'cancelled')
                AND parent.hidden = 0 AND parent.provenance != 'native_plan'
                AND NOT EXISTS (SELECT 1 FROM connect_work w WHERE w.kind='task_turn' AND w.local_id=parent.id))
  AND NOT EXISTS (SELECT 1 FROM connect_work w WHERE w.kind = 'task_turn' AND w.local_id = tasks.id)
  AND NOT EXISTS (SELECT 1 FROM workflow_step_executions e JOIN workflow_instances i ON i.id=e.instance_id
                  WHERE e.task_id=tasks.id AND e.task_id IS NOT i.source_task_id);

-- Extend the durable notification journal without losing delivery receipts.
CREATE TEMP TABLE saved_delegation_notifications AS SELECT * FROM task_delegation_notifications;
CREATE TEMP TABLE saved_delegation_responses AS SELECT * FROM task_delegation_responses;
DROP TABLE task_delegation_responses;
DROP TABLE task_delegation_notifications;
CREATE TABLE task_delegation_notifications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    parent_agent_id TEXT NOT NULL,
    child_task_id TEXT NOT NULL,
    child_attempt_id TEXT NOT NULL,
    child_status TEXT NOT NULL CHECK(child_status IN ('completed', 'blocked', 'cancelled', 'waiting_for_input')),
    message_id INTEGER REFERENCES task_messages(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    decision TEXT CHECK(decision IN ('accept', 'revise', 'wait')),
    review_note TEXT,
    reviewed_at TEXT,
    UNIQUE(parent_task_id, parent_agent_id, child_task_id, child_attempt_id, child_status)
);
CREATE TABLE task_delegation_responses (
    response_attempt_id TEXT NOT NULL REFERENCES work_attempts(id) ON DELETE CASCADE,
    notification_id INTEGER NOT NULL REFERENCES task_delegation_notifications(id) ON DELETE CASCADE,
    PRIMARY KEY(response_attempt_id, notification_id)
);
INSERT INTO task_delegation_notifications
    (id, parent_task_id, parent_agent_id, child_task_id, child_attempt_id, child_status, message_id, created_at)
SELECT id, parent_task_id, parent_agent_id, child_task_id, child_attempt_id, child_status, message_id, created_at
FROM saved_delegation_notifications;
INSERT INTO task_delegation_responses SELECT * FROM saved_delegation_responses;
DROP TABLE saved_delegation_responses;
DROP TABLE saved_delegation_notifications;
