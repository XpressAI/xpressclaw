-- Only newly created notification-only turns may be retired automatically.
-- Existing responses have unknown origin and remain intact on upgrade.
CREATE TABLE task_delegation_automatic_responses (
    response_attempt_id TEXT PRIMARY KEY REFERENCES work_attempts(id) ON DELETE CASCADE
);
