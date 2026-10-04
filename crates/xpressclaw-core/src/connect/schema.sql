CREATE TABLE connect_bindings (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    local_project_id TEXT NOT NULL,
    local_agent_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    active INTEGER NOT NULL,
    binding_json TEXT NOT NULL,
    UNIQUE(instance_id, local_agent_id)
);
CREATE TABLE connect_commands (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    binding_id TEXT NOT NULL REFERENCES connect_bindings(id),
    payload_hash TEXT NOT NULL,
    binding_generation INTEGER NOT NULL,
    conversation_id TEXT,
    turn_id TEXT,
    status TEXT NOT NULL,
    result_text TEXT,
    result_error TEXT,
    acknowledged INTEGER NOT NULL DEFAULT 0,
    lease_until INTEGER NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE UNIQUE INDEX idx_connect_turn ON connect_commands(turn_id);
CREATE INDEX idx_connect_pending ON connect_commands(instance_id, acknowledged);
