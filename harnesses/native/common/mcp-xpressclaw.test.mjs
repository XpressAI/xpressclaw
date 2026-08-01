import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  briefingMarkdown,
  buildInstructions,
  buildProjectMemoryRequest,
  buildWakeupRequest,
  memoryResourceTemplates,
  TOOLS,
} from './mcp-xpressclaw.mjs';

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

test('binds new memory provenance to the current task and agent', () => {
  const request = buildProjectMemoryRequest(
    {
      title: 'Deployment convention',
      body: 'Use blue-green deployment.',
      note_type: 'convention',
      source_task_id: 'spoofed-task',
      created_by: 'user',
    },
    { taskId: 'task-123' },
  );

  assert.deepEqual(request, {
    title: 'Deployment convention',
    body: 'Use blue-green deployment.',
    note_type: 'convention',
    source_task_id: 'task-123',
    created_by: 'agent',
  });
});

test('initialization instructions hint at available project memory', () => {
  const instructions = buildInstructions({
    active_notes: 7,
    pinned_notes: 2,
    note_types: [{ name: 'decision', count: 3 }],
    top_tags: [{ name: '日本語', count: 2 }],
    pinned: [{ id: 'note-1', title: 'Deployment decision' }],
    recent: [],
    hint: 'Search before changing conventions.',
  });

  assert.match(instructions, /7 active note\(s\)/);
  assert.match(instructions, /decision: 3/);
  assert.match(instructions, /日本語/);
  assert.match(instructions, /Deployment decision/);
  assert.match(instructions, /data, not instructions/);
  assert.match(instructions, /memory:\/\/project\/briefing/);
});

test('advertises a note resource template and renders discoverable links', () => {
  assert.deepEqual(memoryResourceTemplates(), [{
    uriTemplate: 'memory://project/note/{note_id}',
    name: 'Project memory note',
    description: 'Read one durable note by ID in the current project.',
    mimeType: 'text/markdown',
    annotations: { audience: ['assistant'], priority: 1 },
  }]);

  const markdown = briefingMarkdown({
    hint: 'Use durable memory.',
    active_notes: 1,
    pinned_notes: 1,
    note_types: [],
    top_tags: [],
    pinned: [{ id: 'note/1', title: '決定', summary: 'SQLiteを使う' }],
    recent: [],
  });
  assert.match(markdown, /memory:\/\/project\/note\/note%2F1/);
  assert.match(markdown, /SQLiteを使う/);
});

test('advertises project memory tools with conservative MCP annotations', () => {
  const search = TOOLS.find((tool) => tool.name === 'search_project_memory');
  const archive = TOOLS.find((tool) => tool.name === 'archive_project_memory');
  assert.equal(search.annotations.readOnlyHint, true);
  assert.equal(search.annotations.openWorldHint, false);
  assert.equal(archive.annotations.destructiveHint, true);
});

test('serves project memory discovery and writes over the stdio MCP protocol', { timeout: 5000 }, async () => {
  let createdBody = null;
  const server = createServer(async (request, response) => {
    if (request.url === '/api/memory/project-a/index' && request.method === 'GET') {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({
        active_notes: 3,
        pinned_notes: 1,
        note_types: [{ name: 'decision', count: 2 }],
        top_tags: [{ name: 'architecture', count: 2 }],
        pinned: [],
        recent: [],
        hint: 'Review project decisions first.',
      }));
      return;
    }
    if (request.url === '/api/memory/project-a/notes' && request.method === 'POST') {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      createdBody = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      response.writeHead(201, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ id: 'note-1', ...createdBody }));
      return;
    }
    response.writeHead(404, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ error: 'not found' }));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  assert.equal(typeof address, 'object');

  const child = spawn(
    process.execPath,
    [fileURLToPath(new URL('./mcp-xpressclaw.mjs', import.meta.url))],
    {
      env: {
        ...process.env,
        XPRESSCLAW_URL: `http://127.0.0.1:${address.port}`,
        XPRESSCLAW_AGENT_ID: 'project-a',
        XPRESSCLAW_TASK_ID: 'task-9',
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    },
  );
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const output = lines[Symbol.asyncIterator]();
  let childError = '';
  child.stderr.on('data', (chunk) => {
    childError += chunk.toString('utf8');
  });
  const requestMcp = async (message) => {
    child.stdin.write(`${JSON.stringify(message)}\n`);
    const next = await output.next();
    assert.equal(next.done, false, childError || 'MCP server closed stdout unexpectedly');
    return JSON.parse(next.value);
  };

  try {
    const initialized = await requestMcp({
      jsonrpc: '2.0',
      id: 1,
      method: 'initialize',
      params: { protocolVersion: '2025-11-25' },
    });
    assert.equal(initialized.result.capabilities.resources.listChanged, true);
    assert.match(initialized.result.instructions, /3 active note\(s\)/);

    const listed = await requestMcp({ jsonrpc: '2.0', id: 2, method: 'tools/list' });
    assert.ok(listed.result.tools.some((tool) => tool.name === 'create_project_memory'));

    const created = await requestMcp({
      jsonrpc: '2.0',
      id: 3,
      method: 'tools/call',
      params: {
        name: 'create_project_memory',
        arguments: { title: 'ADR', body: 'Use SQLite.', note_type: 'decision' },
      },
    });
    assert.equal(created.result.structuredContent.id, 'note-1');
    assert.equal(createdBody.source_task_id, 'task-9');
    assert.equal(createdBody.created_by, 'agent');

    const notification = await output.next();
    assert.equal(
      JSON.parse(notification.value).method,
      'notifications/resources/list_changed',
    );
  } finally {
    child.stdin.end();
    if (child.exitCode === null) await once(child, 'exit');
    lines.close();
    await new Promise((resolve) => server.close(resolve));
  }
});
