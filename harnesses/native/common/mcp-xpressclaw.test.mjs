import assert from 'node:assert/strict';
import { execFile as execFileCallback, spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

import {
  briefingMarkdown,
  buildInstructions,
  buildProjectMemoryRequest,
  buildWakeupRequest,
  memoryResourceTemplates,
  runManagedGitPush,
  TOOLS,
} from './mcp-xpressclaw.mjs';

const execFile = promisify(execFileCallback);

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

test('binds a scheduled wake-up to the conversation lane that armed it', () => {
  const request = buildWakeupRequest(
    {
      name: 'Check review',
      delay_seconds: 60,
      message: 'Check the review and update the conversation.',
    },
    {
      agentId: 'reviewer',
      taskId: '',
      conversationId: 'conversation-123',
    },
  );

  assert.equal(request.continuation_task_id, undefined);
  assert.equal(request.conversation_id, 'conversation-123');
});

test('task wake-ups take precedence for tasks linked to conversations', () => {
  const request = buildWakeupRequest(
    {
      delay_seconds: 60,
      message: 'Continue the task.',
    },
    {
      agentId: 'reviewer',
      taskId: 'task-123',
      conversationId: 'conversation-123',
    },
  );

  assert.equal(request.continuation_task_id, 'task-123');
  assert.equal(request.conversation_id, undefined);
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
  const get = TOOLS.find((tool) => tool.name === 'get_project_memory');
  const archive = TOOLS.find((tool) => tool.name === 'archive_project_memory');
  assert.equal(search.annotations.readOnlyHint, true);
  assert.equal(search.annotations.openWorldHint, false);
  assert.equal(get.annotations.readOnlyHint, false);
  assert.equal(get.annotations.idempotentHint, false);
  assert.equal(archive.annotations.destructiveHint, true);
});

test('local collaboration tools are visible only to explicitly authorized Agent processes', async () => {
  assert.equal(TOOLS.some((tool) => tool.name === 'local_forge_create_repository'), false);
  const child = spawn(
    process.execPath,
    [fileURLToPath(new URL('./mcp-xpressclaw.mjs', import.meta.url))],
    {
      env: {
        ...process.env,
        XPRESSCLAW_URL: 'http://127.0.0.1:1',
        XPRESSCLAW_AGENT_ID: 'authorized-agent',
        XPRESSCLAW_LOCAL_COLLABORATION: '1',
        XPRESSCLAW_COLLABORATION_TOKEN: 'scoped-capability',
      },
      stdio: ['pipe', 'pipe', 'pipe'],
    },
  );
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const output = lines[Symbol.asyncIterator]();
  child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' })}\n`);
  const listed = JSON.parse((await output.next()).value);
  const names = listed.result.tools.map((tool) => tool.name);
  assert.ok(names.includes('local_forge_create_repository'));
  assert.ok(names.includes('local_forge_push_branch'));
  assert.ok(names.includes('local_build_trigger'));
  child.stdin.end();
  if (child.exitCode === null) await once(child, 'exit');
  lines.close();
});

test('managed forge pushes disable repository and configured Git hooks', { timeout: 10000 }, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'xpressclaw-managed-push-'));
  const checkout = path.join(root, 'checkout');
  const remote = path.join(root, 'remote.git');
  const configuredHooks = path.join(root, 'configured-hooks');
  const repositoryMarker = path.join(root, 'repository-hook-ran');
  const configuredMarker = path.join(root, 'configured-hook-ran');
  const token = 'malicious-hook-must-not-see-this-token';
  const git = (argumentsValue, cwd = checkout) => execFile('git', argumentsValue, { cwd });

  try {
    await mkdir(checkout);
    await mkdir(configuredHooks);
    await git(['init', '-b', 'main']);
    await git(['config', 'user.name', 'XpressClaw test']);
    await git(['config', 'user.email', 'test@localhost']);
    await writeFile(path.join(checkout, 'README.md'), '# fixture\n');
    await git(['add', 'README.md']);
    await git(['commit', '-m', 'fixture']);
    await execFile('git', ['init', '--bare', remote]);

    const repositoryHook = path.join(checkout, '.git', 'hooks', 'pre-push');
    await writeFile(
      repositoryHook,
      `#!/bin/sh\nprintf '%s' "$XPRESSCLAW_GIT_TOKEN"\ntouch ${JSON.stringify(repositoryMarker)}\n`,
    );
    await chmod(repositoryHook, 0o700);
    const configuredHook = path.join(configuredHooks, 'pre-push');
    await writeFile(
      configuredHook,
      `#!/bin/sh\nprintf '%s' "$XPRESSCLAW_GIT_TOKEN" >&2\ntouch ${JSON.stringify(configuredMarker)}\n`,
    );
    await chmod(configuredHook, 0o700);
    const remoteHook = path.join(remote, 'hooks', 'pre-receive');
    await writeFile(remoteHook, `#!/bin/sh\necho ${JSON.stringify(token)} >&2\n`);
    await chmod(remoteHook, 0o700);

    const output = await runManagedGitPush({
      directory: checkout,
      remote,
      branch: 'main',
      username: 'xpressclaw-agent',
      token,
    });
    assert.equal(output.includes(token), false);
    assert.match(output, /\[REDACTED\]/);
    await assert.rejects(readFile(repositoryMarker), { code: 'ENOENT' });

    await git(['config', 'core.hooksPath', configuredHooks]);
    await writeFile(path.join(checkout, 'README.md'), '# configured hook fixture\n');
    await git(['add', 'README.md']);
    await git(['commit', '-m', 'exercise configured hook suppression']);
    const configuredOutput = await runManagedGitPush({
      directory: checkout,
      remote,
      branch: 'main',
      username: 'xpressclaw-agent',
      token,
    });
    assert.equal(configuredOutput.includes(token), false);
    assert.match(configuredOutput, /\[REDACTED\]/);
    await assert.rejects(readFile(configuredMarker), { code: 'ENOENT' });

    await git(['commit', '--amend', '--no-edit']);
    await runManagedGitPush({
      directory: checkout,
      remote,
      branch: 'main',
      username: 'xpressclaw-agent',
      token,
      forceWithLease: true,
    });

    await writeFile(remoteHook, `#!/bin/sh\necho ${JSON.stringify(token)} >&2\nexit 1\n`);
    await writeFile(path.join(checkout, 'README.md'), '# changed fixture\n');
    await git(['add', 'README.md']);
    await git(['commit', '-m', 'exercise failed push redaction']);
    await assert.rejects(
      runManagedGitPush({
        directory: checkout,
        remote,
        branch: 'main',
        username: 'xpressclaw-agent',
        token,
      }),
      (error) => {
        assert.equal(error.message.includes(token), false);
        assert.match(error.message, /\[REDACTED\]/);
        return true;
      },
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('managed forge pushes do not execute configured credential helpers', { timeout: 10000 }, async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'xpressclaw-managed-credentials-'));
  const checkout = path.join(root, 'checkout');
  const marker = path.join(root, 'credential-helper-ran');
  const globalMarker = path.join(root, 'global-credential-helper-ran');
  const globalConfig = path.join(root, 'malicious-global-gitconfig');
  const token = 'credential-helper-must-not-see-this-token';
  const server = createServer((_request, response) => {
    response.writeHead(401, { 'www-authenticate': 'Basic realm="test"' });
    response.end('authentication required');
  });

  try {
    await mkdir(checkout);
    await execFile('git', ['init', '-b', 'main'], { cwd: checkout });
    await execFile('git', ['config', 'user.name', 'XpressClaw test'], { cwd: checkout });
    await execFile('git', ['config', 'user.email', 'test@localhost'], { cwd: checkout });
    await execFile(
      'git',
      ['config', 'credential.helper', `!touch ${JSON.stringify(marker)}; echo username=stolen; echo password=$XPRESSCLAW_GIT_TOKEN`],
      { cwd: checkout },
    );
    await writeFile(path.join(checkout, 'README.md'), '# fixture\n');
    await execFile('git', ['add', 'README.md'], { cwd: checkout });
    await execFile('git', ['commit', '-m', 'fixture'], { cwd: checkout });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    const address = server.address();
    assert.equal(typeof address, 'object');

    await assert.rejects(
      runManagedGitPush({
        directory: checkout,
        remote: `http://127.0.0.1:${address.port}/repository.git`,
        branch: 'main',
        username: 'xpressclaw-agent',
        token,
      }),
      (error) => {
        assert.equal(error.message.includes(token), false);
        return true;
      },
    );
    await assert.rejects(readFile(marker), { code: 'ENOENT' });

    await execFile('git', ['config', '--unset-all', 'credential.helper'], { cwd: checkout });
    await execFile(
      'git',
      ['config', '--file', globalConfig, 'credential.helper', `!touch ${JSON.stringify(globalMarker)}; echo username=stolen; echo password=$XPRESSCLAW_GIT_TOKEN`],
    );
    const previousGlobalConfig = process.env.GIT_CONFIG_GLOBAL;
    process.env.GIT_CONFIG_GLOBAL = globalConfig;
    try {
      await assert.rejects(runManagedGitPush({
        directory: checkout,
        remote: `http://127.0.0.1:${address.port}/repository.git`,
        branch: 'main',
        username: 'xpressclaw-agent',
        token,
      }));
    } finally {
      if (previousGlobalConfig === undefined) delete process.env.GIT_CONFIG_GLOBAL;
      else process.env.GIT_CONFIG_GLOBAL = previousGlobalConfig;
    }
    await assert.rejects(readFile(globalMarker), { code: 'ENOENT' });
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await rm(root, { recursive: true, force: true });
  }
});

test('serves project memory discovery and writes over the stdio MCP protocol', { timeout: 5000 }, async () => {
  let createdBody = null;
  const controlTokens = [];
  const server = createServer(async (request, response) => {
    controlTokens.push(request.headers['x-xpressclaw-internal-token']);
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
        XPRESSCLAW_CONTROL_TOKEN: 'internal-secret',
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
    assert.ok(controlTokens.length > 0);
    assert.ok(controlTokens.every((token) => token === 'internal-secret'));
  } finally {
    child.stdin.end();
    if (child.exitCode === null) await once(child, 'exit');
    lines.close();
    await new Promise((resolve) => server.close(resolve));
  }
});

test('conversation tools publish files, download attachments, and create linked work', { timeout: 5000 }, async () => {
  const workspace = await mkdtemp(path.join(tmpdir(), 'xpressclaw-conversation-'));
  await writeFile(path.join(workspace, 'report.md'), '# Report\nUseful evidence.\n');
  let publishedBody = null;
  let taskBody = null;
  const controlTokens = [];
  const server = createServer(async (request, response) => {
    controlTokens.push(request.headers['x-xpressclaw-internal-token']);
    if (request.url === '/api/memory/project-a/index' && request.method === 'GET') {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ active_notes: 0, pinned_notes: 0, note_types: [], top_tags: [], pinned: [], recent: [], hint: '' }));
      return;
    }
    if (request.url === '/api/conversations/conversation-a/agent-messages' && request.method === 'POST') {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      publishedBody = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      response.writeHead(201, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ message: { id: 1 }, queued_agents: [] }));
      return;
    }
    if (request.url === '/api/conversations/conversation-a/attachments/file-1' && request.method === 'GET') {
      response.writeHead(200, { 'content-type': 'text/markdown' });
      response.end('# Published finding\n');
      return;
    }
    if (request.url === '/api/conversations/conversation-a/tasks' && request.method === 'POST') {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      taskBody = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      response.writeHead(201, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ id: 'task-10', ...taskBody }));
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
        XPRESSCLAW_CONTROL_TOKEN: 'internal-secret',
        XPRESSCLAW_AGENT_ID: 'atlas',
        XPRESSCLAW_TASK_ID: 'task-9',
        XPRESSCLAW_PROJECT_ID: 'project-a',
        XPRESSCLAW_CONVERSATION_ID: 'conversation-a',
        XPRESSCLAW_WORKSPACE: workspace,
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
    const initialized = await requestMcp({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25' } });
    assert.match(initialized.result.instructions, /linked to a project conversation/);
    assert.match(initialized.result.instructions, /normal final response is automatically delivered to this project conversation/);
    assert.match(initialized.result.instructions, /Reserve send_conversation_message for genuine interim updates or publishing workspace files while you continue working/);
    assert.match(initialized.result.instructions, /Never use the tool to duplicate your final response/);
    const listed = await requestMcp({ jsonrpc: '2.0', id: 2, method: 'tools/list' });
    assert.deepEqual(
      listed.result.tools
        .filter((tool) => tool.name.includes('conversation'))
        .map((tool) => tool.name),
      ['send_conversation_message', 'download_conversation_attachment', 'create_conversation_task'],
    );
    const sendConversationMessage = listed.result.tools.find((tool) => tool.name === 'send_conversation_message');
    assert.match(sendConversationMessage.description, /genuine interim update or publish workspace files/);
    assert.match(sendConversationMessage.description, /normal final response is delivered automatically/);
    assert.match(sendConversationMessage.description, /never use this tool to duplicate it/);

    await requestMcp({
      jsonrpc: '2.0',
      id: 3,
      method: 'tools/call',
      params: { name: 'send_conversation_message', arguments: { content: 'Research complete.', files: ['report.md'] } },
    });
    assert.equal(publishedBody.agent_id, 'atlas');
    assert.equal(publishedBody.source_task_id, 'task-9');
    assert.equal(Buffer.from(publishedBody.attachments[0].data, 'base64').toString('utf8'), '# Report\nUseful evidence.\n');

    const downloaded = await requestMcp({
      jsonrpc: '2.0',
      id: 4,
      method: 'tools/call',
      params: { name: 'download_conversation_attachment', arguments: { attachment_id: 'file-1', file_name: '../finding.md' } },
    });
    const downloadedPath = downloaded.result.structuredContent.path;
    assert.equal(path.dirname(downloadedPath), path.join(workspace, '.xpressclaw', 'conversations', 'conversation-a'));
    assert.equal(await readFile(downloadedPath, 'utf8'), '# Published finding\n');

    await requestMcp({
      jsonrpc: '2.0',
      id: 5,
      method: 'tools/call',
      params: { name: 'create_conversation_task', arguments: { title: 'Continue research', description: 'Check the edge cases.' } },
    });
    assert.equal(taskBody.agent_id, 'atlas');
    assert.equal(taskBody.creator_agent_id, 'atlas');
    assert.equal(taskBody.project_id, 'project-a');
    assert.ok(controlTokens.length > 0);
    assert.ok(controlTokens.every((token) => token === 'internal-secret'));
  } finally {
    child.stdin.end();
    if (child.exitCode === null) await once(child, 'exit');
    lines.close();
    await new Promise((resolve) => server.close(resolve));
    await rm(workspace, { recursive: true, force: true });
  }
});
