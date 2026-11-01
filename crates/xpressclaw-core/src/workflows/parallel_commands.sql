ALTER TABLE workflow_step_executions ADD COLUMN parent_execution_id TEXT REFERENCES workflow_step_executions(id) ON DELETE CASCADE;
CREATE INDEX idx_workflow_step_parent ON workflow_step_executions(parent_execution_id);
ALTER TABLE workflow_step_executions ADD COLUMN command_started_at TEXT;
ALTER TABLE workflow_step_executions ADD COLUMN command_result TEXT;
ALTER TABLE workflow_instances ADD COLUMN current_execution_id TEXT;
