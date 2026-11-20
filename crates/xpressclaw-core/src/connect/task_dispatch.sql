ALTER TABLE connect_commands ADD COLUMN task_id TEXT;
ALTER TABLE connect_commands ADD COLUMN attempt_id TEXT;
CREATE UNIQUE INDEX idx_connect_attempt ON connect_commands(attempt_id);

-- One local work item per platform identity, with a separate receipt per turn.
CREATE TABLE connect_work (
    instance_id TEXT NOT NULL,
    binding_id TEXT NOT NULL REFERENCES connect_bindings(id),
    kind TEXT NOT NULL CHECK (kind IN ('chat_turn', 'task_turn')),
    work_id TEXT NOT NULL,
    local_id TEXT NOT NULL,
    PRIMARY KEY (instance_id, binding_id, kind, work_id),
    UNIQUE(local_id)
);
