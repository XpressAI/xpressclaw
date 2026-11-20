CREATE TABLE task_delegation_notifications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    parent_agent_id TEXT NOT NULL,
    -- Keep the historical notification even if the child is subsequently deleted.
    child_task_id TEXT NOT NULL,
    child_attempt_id TEXT NOT NULL,
    child_status TEXT NOT NULL CHECK(child_status IN ('blocked', 'cancelled', 'waiting_for_input')),
    message_id INTEGER REFERENCES task_messages(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(parent_task_id, parent_agent_id, child_task_id, child_attempt_id, child_status)
);
CREATE TABLE task_delegation_responses (
    response_attempt_id TEXT NOT NULL REFERENCES work_attempts(id) ON DELETE CASCADE,
    notification_id INTEGER NOT NULL REFERENCES task_delegation_notifications(id) ON DELETE CASCADE,
    PRIMARY KEY(response_attempt_id, notification_id)
);
