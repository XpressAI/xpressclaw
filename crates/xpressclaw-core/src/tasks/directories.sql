CREATE TABLE task_directories (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    baseline TEXT,
    previous_files TEXT NOT NULL DEFAULT '[]',
    files TEXT NOT NULL DEFAULT '[]',
    notice TEXT,
    UNIQUE(task_id, agent_id, path)
);
