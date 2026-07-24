import assert from 'node:assert/strict';
import test from 'node:test';

import { buildWakeupRequest } from './mcp-xpressclaw.mjs';

test('binds a scheduled wake-up to the task that armed it', () => {
  const request = buildWakeupRequest(
    {
      name: 'Check CI',
      delay_seconds: 300,
      message: 'Inspect the checks and report the result.',
    },
    {
      agentId: 'xpressclaw-codex',
      taskId: 'task-123',
    },
  );

  assert.deepEqual(request, {
    name: 'Check CI',
    agent_id: 'xpressclaw-codex',
    title: 'Check CI',
    description: 'Inspect the checks and report the result.',
    continuation_task_id: 'task-123',
    delay_seconds: 300,
  });
});

test('keeps standalone compatibility when no task identity is available', () => {
  const request = buildWakeupRequest(
    {
      run_at: '2026-07-24T09:00:00Z',
      message: 'Run the report.',
    },
    {
      agentId: 'atlas',
      taskId: '',
    },
  );

  assert.equal(request.continuation_task_id, undefined);
  assert.equal(request.run_at, '2026-07-24T09:00:00Z');
});

test('rejects ambiguous wake-up deadlines', () => {
  assert.throws(
    () => buildWakeupRequest(
      {
        delay_seconds: 60,
        run_at: '2026-07-24T09:00:00Z',
        message: 'Invalid',
      },
      {
        agentId: 'atlas',
        taskId: 'task-123',
      },
    ),
    /provide exactly one/,
  );
});
