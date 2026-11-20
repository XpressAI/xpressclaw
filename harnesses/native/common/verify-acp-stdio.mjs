#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { access, mkdtemp, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const REQUEST_TIMEOUT_MS = 90_000;
const EXIT_TIMEOUT_MS = 10_000;
const STDERR_LIMIT = 32 * 1024;

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

function completionChunk(delta, finishReason = null, usage) {
  return {
    id: 'xpressclaw-dsh-smoke',
    object: 'chat.completion.chunk',
    created: 1,
    model: 'deepseek-v4-flash-vision-exp',
    choices: [{ index: 0, delta, finish_reason: finishReason }],
    ...(usage === undefined ? {} : { usage }),
  };
}

function finishSse(response, chunks) {
  response.writeHead(200, {
    'content-type': 'text/event-stream',
    'cache-control': 'no-cache',
    connection: 'close',
  });
  for (const chunk of chunks) response.write(`data: ${JSON.stringify(chunk)}\n\n`);
  response.end('data: [DONE]\n\n');
}

function toolCall(index, id, name, args) {
  return {
    index,
    id,
    type: 'function',
    function: { name, arguments: JSON.stringify(args) },
  };
}

async function readRequestBody(request) {
  let body = '';
  for await (const chunk of request) body += chunk;
  return body;
}

async function startFakeProvider({ fixturePath, outsidePath }) {
  const cancellationStarted = deferred();
  const cancellationAborted = deferred();
  const state = {
    modelSteps: 0,
    sawImage: false,
    fileFallbacks: 0,
    permissionFlowCompleted: false,
    cancellationStarted: cancellationStarted.promise,
    cancellationAborted: cancellationAborted.promise,
    error: null,
  };
  const command = `printf 'allowed by ACP permission' > '${outsidePath}'`;

  const server = createServer((request, response) => {
    void (async () => {
      const url = new URL(request.url ?? '/', 'http://127.0.0.1');
      if (url.pathname === '/files') {
        await readRequestBody(request);
        state.fileFallbacks += 1;
        response.writeHead(501, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ error: { message: 'inline images only in smoke fixture' } }));
        return;
      }
      invariant(url.pathname === '/chat/completions', `unexpected provider path: ${url.pathname}`);
      invariant(request.headers.authorization === 'Bearer sk-xpressclaw-image-smoke-not-a-real-key', 'adapter omitted the isolated bearer credential');
      const body = JSON.parse(await readRequestBody(request));
      const serialized = JSON.stringify(body.messages ?? []);
      const system = String(body.messages?.find((message) => message.role === 'system')?.content ?? '');
      if (system.includes('Create a concise title for an AI coding-assistant session')) {
        finishSse(response, [completionChunk({ content: 'ACP compatibility smoke' }, 'stop', {
          prompt_tokens: 4,
          completion_tokens: 3,
        })]);
        return;
      }
      const cancellation = serialized.includes('Wait until XpressClaw cancels this response');
      if (cancellation) {
        response.writeHead(200, {
          'content-type': 'text/event-stream',
          'cache-control': 'no-cache',
          connection: 'close',
        });
        response.write(`data: ${JSON.stringify(completionChunk({ reasoning_content: 'Waiting for cancellation.' }))}\n\n`);
        response.once('close', () => cancellationAborted.resolve());
        cancellationStarted.resolve();
        return;
      }

      if (serialized.includes('"image_url"') && serialized.includes('data:image/')) {
        state.sawImage = true;
        finishSse(response, [completionChunk({ content: 'Image prompt accepted.' }, 'stop', {
          prompt_tokens: 4,
          completion_tokens: 3,
        })]);
        return;
      }

      if (state.modelSteps === 0) {
        const tools = new Set((body.tools ?? []).map((entry) => entry.function?.name));
        for (const name of ['todo_write', 'read', 'write', 'bash']) {
          invariant(tools.has(name), `DeepSeek Harness did not expose the ${name} tool (found: ${[...tools].sort().join(', ')}; request keys: ${Object.keys(body).sort().join(', ')}; system tail: ${system.slice(-500)})`);
        }
        finishSse(response, [completionChunk({
          reasoning_content: 'Plan, inspect, and update the fixture.',
          tool_calls: [
            toolCall(0, 'smoke-plan', 'todo_write', {
              todos: [{ content: 'Verify the ACP bridge', status: 'in_progress' }],
            }),
            toolCall(1, 'smoke-read', 'read', { file_path: fixturePath }),
          ],
        }, 'tool_calls')]);
      } else if (state.modelSteps === 1) {
        finishSse(response, [completionChunk({
          tool_calls: [toolCall(0, 'smoke-write', 'write', {
            file_path: fixturePath,
            content: 'updated through DeepSeek Harness ACP\n',
          })],
        }, 'tool_calls')]);
      } else if (state.modelSteps === 2) {
        finishSse(response, [completionChunk({
          tool_calls: [toolCall(0, 'smoke-denied', 'bash', {
            command,
            description: 'Attempt write beyond session workspace',
          })],
        }, 'tool_calls')]);
      } else if (state.modelSteps === 3) {
        finishSse(response, [completionChunk({
          tool_calls: [toolCall(0, 'smoke-approved', 'bash', {
            command,
            description: 'Retry approved write beyond workspace',
            sandbox_permissions: 'danger-full-access',
            justification: 'Verify the ACP permission request round trip.',
          })],
        }, 'tool_calls')]);
        state.permissionFlowCompleted = true;
      } else {
        finishSse(response, [
          completionChunk({ reasoning_content: 'All compatibility checks completed.' }),
          completionChunk({ content: 'DeepSeek Harness ' }),
          completionChunk({ content: 'ACP bridge verified.' }, 'stop', {
            prompt_tokens: 20,
            completion_tokens: 6,
            prompt_cache_hit_tokens: 2,
          }),
        ]);
      }
      state.modelSteps += 1;
    })().catch((error) => {
      state.error = error;
      if (!response.headersSent) response.writeHead(500, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ error: { message: error.message } }));
    });
  });

  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  const address = server.address();
  invariant(address && typeof address === 'object', 'fake provider did not bind a TCP port');
  return {
    baseUrl: `http://127.0.0.1:${address.port}`,
    state,
    close: () => {
      server.closeAllConnections();
      return new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
    },
  };
}

class AcpClient {
  constructor(command, args, environment) {
    this.child = spawn(command, args, {
      env: environment,
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    this.nextId = 1;
    this.pending = new Map();
    this.notifications = [];
    this.permissionRequests = [];
    this.stdout = '';
    this.stderr = '';

    this.child.stdout.setEncoding('utf8');
    this.child.stderr.setEncoding('utf8');
    this.child.stderr.on('data', (chunk) => {
      this.stderr = `${this.stderr}${chunk}`.slice(-STDERR_LIMIT);
    });
    this.child.stdout.on('data', (chunk) => {
      this.stdout += chunk;
      let newline = this.stdout.indexOf('\n');
      while (newline >= 0) {
        const line = this.stdout.slice(0, newline).trim();
        this.stdout = this.stdout.slice(newline + 1);
        if (line) this.dispatch(line);
        newline = this.stdout.indexOf('\n');
      }
    });
    this.child.on('error', (error) => this.rejectAll(error));
    this.child.on('exit', (code, signal) => {
      this.rejectAll(new Error(
        `ACP server exited before replying (code=${String(code)}, signal=${String(signal)})\n${this.stderr}`,
      ));
    });
  }

  dispatch(line) {
    let message;
    try {
      message = JSON.parse(line);
    } catch (error) {
      this.rejectAll(new Error(`ACP server wrote non-JSON stdout: ${line}\n${String(error)}`));
      return;
    }
    if (typeof message.method === 'string') {
      this.notifications.push(message);
      if (message.id !== undefined && message.method === 'session/request_permission') {
        this.permissionRequests.push(message.params);
        const allow = message.params?.options?.find((option) => option.optionId === 'allow-once');
        if (!allow) {
          this.child.stdin.write(`${JSON.stringify({
            jsonrpc: '2.0', id: message.id,
            error: { code: -32602, message: 'adapter omitted allow-once permission option' },
          })}\n`);
        } else {
          this.child.stdin.write(`${JSON.stringify({
            jsonrpc: '2.0', id: message.id,
            result: { outcome: { outcome: 'selected', optionId: allow.optionId } },
          })}\n`);
        }
      }
      return;
    }
    const pending = this.pending.get(message.id);
    if (pending) {
      this.pending.delete(message.id);
      clearTimeout(pending.timer);
      if (message.error) {
        const error = new Error(message.error.message ?? 'ACP request failed');
        error.code = message.error.code;
        error.data = message.error.data;
        pending.reject(error);
      } else {
        pending.resolve(message.result);
      }
      return;
    }
  }

  rejectAll(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  request(method, params, timeoutMs = REQUEST_TIMEOUT_MS) {
    const id = this.nextId++;
    const response = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out\n${this.stderr}`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
    });
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    return response;
  }

  notify(method, params) {
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', method, params })}\n`);
  }

  updates(sessionId) {
    return this.notifications
      .filter((message) => message.method === 'session/update' && message.params?.sessionId === sessionId)
      .map((message) => message.params.update);
  }

  async close() {
    if (this.child.exitCode !== null || this.child.signalCode !== null) return;
    const exited = new Promise((resolve) => this.child.once('exit', resolve));
    this.child.stdin.end();
    const graceful = await Promise.race([
      exited.then(() => true),
      new Promise((resolve) => setTimeout(() => resolve(false), EXIT_TIMEOUT_MS)),
    ]);
    if (graceful) return;
    this.child.kill('SIGTERM');
    const terminated = await Promise.race([
      exited.then(() => true),
      new Promise((resolve) => setTimeout(() => resolve(false), EXIT_TIMEOUT_MS)),
    ]);
    if (!terminated) this.child.kill('SIGKILL');
    invariant(terminated, `ACP server did not stop after stdin closed or SIGTERM\n${this.stderr}`);
  }
}

async function waitForFile(path, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      await access(path);
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  throw new Error(`MCP server was not initialized: ${path}`);
}

async function waitForPromise(promise, message, timeoutMs = 10_000) {
  const reached = await Promise.race([
    promise.then(() => true),
    new Promise((resolve) => setTimeout(() => resolve(false), timeoutMs)),
  ]);
  invariant(reached, message);
}

function initialize(client) {
  return client.request('initialize', {
    protocolVersion: 1,
    clientInfo: { name: 'xpressclaw-runner-verifier', version: '1' },
    clientCapabilities: {
      session: { configOptions: { boolean: {} } },
      elicitation: { form: {} },
    },
  });
}

function assertInitialize(result) {
  invariant(result?.protocolVersion === 1, 'adapter did not negotiate ACP protocol v1');
  invariant(result?.agentInfo?.name === 'dsh-acp', 'adapter returned unexpected agent metadata');
  invariant(result?.agentCapabilities?.loadSession === true, 'adapter did not advertise session loading');
  invariant(
    result?.agentCapabilities?.promptCapabilities?.image === true,
    'adapter did not advertise image prompts',
  );
  invariant(
    result?.authMethods?.some((method) => String(method.id).startsWith('api-key')),
    'adapter did not advertise API-key authentication',
  );
}

function assertSessionControls(session) {
  const modeIds = session?.modes?.availableModes?.map((mode) => mode.id) ?? [];
  for (const mode of ['read-only', 'workspace-write', 'danger-full-access']) {
    invariant(modeIds.includes(mode), `adapter did not advertise ${mode} mode`);
  }
  const optionIds = new Set((session?.configOptions ?? []).map((option) => option.id));
  for (const option of ['mode', 'model', 'effort', 'collaboration_mode']) {
    invariant(optionIds.has(option), `adapter did not advertise ${option} config option`);
  }
}

async function assertStatusPrompt(client, sessionId) {
  const result = await client.request('session/prompt', {
    sessionId,
    prompt: [{ type: 'text', text: '/status' }],
  });
  invariant(result?.stopReason === 'end_turn', '/status did not complete without a model call');
  const text = client.updates(sessionId)
    .filter((update) => update?.sessionUpdate === 'agent_message_chunk')
    .map((update) => update.content?.text ?? '')
    .join('');
  invariant(text.includes('dsh-acp'), '/status did not emit an adapter status message');
}

async function main() {
  const [command, ...args] = process.argv.slice(2);
  if (!command) throw new Error('usage: verify-acp-stdio.mjs <command> [args...]');

  const root = await mkdtemp(join(tmpdir(), 'xpressclaw-dsh-acp-'));
  const home = join(root, 'home');
  const sessionRoot = join(home, 'acp-sessions');
  const firstWorkspace = join(root, 'task-workspace');
  const secondWorkspace = join(root, 'conversation-workspace');
  const mcpMarker = join(root, 'mcp-initialized');
  const mcpFixture = join(root, 'mcp-fixture.mjs');
  const changedFile = join(firstWorkspace, 'fixture.txt');
  const approvedFile = join(root, 'approved-outside-workspace.txt');
  await Promise.all([
    mkdir(home, { recursive: true }),
    mkdir(sessionRoot, { recursive: true }),
    mkdir(firstWorkspace, { recursive: true }),
    mkdir(secondWorkspace, { recursive: true }),
  ]);
  await writeFile(mcpFixture, `
import { writeFileSync } from 'node:fs';
process.stdin.setEncoding('utf8');
let buffer = '';
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  let newline = buffer.indexOf('\\n');
  while (newline >= 0) {
    const line = buffer.slice(0, newline).trim();
    buffer = buffer.slice(newline + 1);
    if (line) {
      const request = JSON.parse(line);
      if (request.method === 'initialize') writeFileSync(process.env.XPRESSCLAW_DSH_MCP_MARKER, 'ready');
      if (request.id !== undefined) {
        const result = request.method === 'initialize'
          ? { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'xpressclaw-smoke', version: '1' } }
          : request.method === 'tools/list' ? { tools: [] } : {};
        process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\\n');
      }
    }
    newline = buffer.indexOf('\\n');
  }
});
`);
  await writeFile(changedFile, 'original fixture\n');
  const provider = await startFakeProvider({ fixturePath: changedFile, outsidePath: approvedFile });

  const environment = { ...process.env };
  for (const key of ['DEEPSEEK_API_KEY', 'ANTHROPIC_API_KEY', 'OPENAI_API_KEY']) delete environment[key];
  Object.assign(environment, {
    DSH_HOME: home,
    DSH_SESSION_ROOT: sessionRoot,
    DSH_ACP_WORKSPACE: firstWorkspace,
    DSH_ACP_CACHE_DIR: join(root, 'runtime-cache'),
    DEEPSEEK_BASE_URL: provider.baseUrl,
    NO_BROWSER: '1',
  });

  let client = new AcpClient(command, args, environment);
  try {
    assertInitialize(await initialize(client));
    let authError;
    try {
      await client.request('session/new', { cwd: firstWorkspace, mcpServers: [] });
    } catch (error) {
      authError = error;
    }
    invariant(authError?.code === -32000, 'missing credentials did not return ACP auth_required');
    await client.request('authenticate', {
      methodId: 'api-key',
      _meta: { 'api-key': { apiKey: 'sk-xpressclaw-image-smoke-not-a-real-key' } },
    });
    const credential = await stat(join(home, '.credentials.yaml'));
    invariant((credential.mode & 0o777) === 0o600, 'adapter credential store is not mode 600');

    const first = await client.request('session/new', {
      cwd: firstWorkspace,
      mcpServers: [{
        name: 'xpressclaw-smoke',
        command: process.execPath,
        args: [mcpFixture],
        env: [{ name: 'XPRESSCLAW_DSH_MCP_MARKER', value: mcpMarker }],
      }],
    });
    const second = await client.request('session/new', { cwd: secondWorkspace, mcpServers: [] });
    invariant(first?.sessionId && second?.sessionId, 'adapter did not create both retained lanes');
    invariant(first.sessionId !== second.sessionId, 'adapter reused one session across two lanes');
    assertSessionControls(first);
    await waitForFile(mcpMarker);
    await client.request('session/set_mode', { sessionId: first.sessionId, modeId: 'workspace-write' });
    await client.request('session/set_config_option', {
      sessionId: first.sessionId,
      configId: 'collaboration_mode',
      value: 'plan',
    });
    await client.request('session/set_config_option', {
      sessionId: first.sessionId,
      configId: 'agent',
      value: 'standard',
    });
    await client.request('session/set_config_option', {
      sessionId: first.sessionId,
      configId: 'effort',
      value: 'high',
    });
    await client.request('session/set_config_option', {
      sessionId: first.sessionId,
      configId: 'model',
      value: 'deepseek-v4-flash-vision-exp',
    });
    await assertStatusPrompt(client, first.sessionId);
    await assertStatusPrompt(client, second.sessionId);

    const imageResult = await client.request('session/prompt', {
      sessionId: first.sessionId,
      prompt: [
        { type: 'text', text: 'Exercise the ACP image-prompt path.' },
        {
          type: 'image',
          mimeType: 'image/gif',
          data: 'R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==',
        },
      ],
    });
    invariant(imageResult?.stopReason === 'end_turn', 'ACP image prompt did not complete');
    await client.request('session/set_config_option', {
      sessionId: first.sessionId,
      configId: 'model',
      value: 'deepseek-v4-pro',
    });
    const modelResult = await client.request('session/prompt', {
      sessionId: first.sessionId,
      prompt: [{ type: 'text', text: 'Exercise the real ACP streaming, tool, plan, diff, and permission path.' }],
    });
    invariant(modelResult?.stopReason === 'end_turn', 'fake-provider ACP turn did not complete');
    invariant(provider.state.error === null, `fake provider failed: ${String(provider.state.error)}`);
    invariant(provider.state.sawImage, 'ACP image prompt did not reach the vision-model request');
    invariant(provider.state.fileFallbacks === 1, 'image upload did not exercise the safe inline fallback');
    invariant(provider.state.permissionFlowCompleted, 'fake model did not reach the permission flow');
    invariant(client.permissionRequests.length === 1, 'adapter did not issue exactly one ACP permission request');

    const updates = client.updates(first.sessionId);
    const toolCalls = updates.filter((update) => update?.sessionUpdate === 'tool_call');
    for (const name of ['todo_write', 'read', 'write', 'bash']) {
      invariant(toolCalls.some((update) => update.name === name), `ACP stream omitted the ${name} tool call`);
    }
    invariant(
      updates.some((update) => update?.sessionUpdate === 'plan'
        && update.entries?.some((entry) => entry.content === 'Verify the ACP bridge')),
      'ACP stream omitted the plan update',
    );
    invariant(
      updates.some((update) => update?.sessionUpdate === 'tool_call_update'
        && update.content?.some((content) => content.type === 'diff' && content.path === changedFile)),
      'ACP stream omitted the file diff',
    );
    invariant(
      updates.some((update) => update?.sessionUpdate === 'agent_thought_chunk'
        && update.content?.text?.includes('compatibility checks')),
      'ACP stream omitted reasoning content',
    );
    invariant(
      updates.some((update) => update?.sessionUpdate === 'agent_message_chunk'
        && update.content?.text?.includes('ACP bridge verified')),
      'ACP stream omitted final message content',
    );
    invariant(await readFile(changedFile, 'utf8') === 'updated through DeepSeek Harness ACP\n', 'write tool did not update the workspace fixture');
    invariant(await readFile(approvedFile, 'utf8') === 'allowed by ACP permission', 'approved tool call did not run outside the workspace');

    const cancelledPrompt = client.request('session/prompt', {
      sessionId: second.sessionId,
      prompt: [{ type: 'text', text: 'Wait until XpressClaw cancels this response.' }],
    });
    await waitForPromise(provider.state.cancellationStarted, 'fake provider never began the cancellable stream');
    client.notify('session/cancel', { sessionId: second.sessionId });
    const cancelled = await cancelledPrompt;
    invariant(cancelled?.stopReason === 'cancelled', 'active ACP cancellation did not report cancelled');
    await waitForPromise(provider.state.cancellationAborted, 'adapter did not abort the provider stream');
    await assertStatusPrompt(client, second.sessionId);
    await client.request('session/set_mode', { sessionId: first.sessionId, modeId: 'read-only' });
    let listed = { sessions: [] };
    for (let attempt = 0; attempt < 40; attempt += 1) {
      listed = await client.request('session/list', {});
      const listedIds = new Set((listed?.sessions ?? []).map((session) => session.sessionId));
      if (listedIds.has(first.sessionId) && listedIds.has(second.sessionId)) break;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    const listedIds = new Set((listed?.sessions ?? []).map((session) => session.sessionId));
    invariant(
      listedIds.has(first.sessionId) && listedIds.has(second.sessionId),
      `session/list omitted a live lane: ${JSON.stringify(listed)}`,
    );

    await client.close();
    client = new AcpClient(command, args, environment);
    assertInitialize(await initialize(client));
    const loaded = await client.request('session/load', {
      sessionId: first.sessionId,
      cwd: firstWorkspace,
      mcpServers: [],
    });
    invariant(loaded?.modes?.currentModeId === 'read-only', 'session mode did not persist across restart');
    await assertStatusPrompt(client, first.sessionId);
    await client.close();
  } finally {
    await client.close().catch(() => {});
    await provider.close().catch(() => {});
    if (process.env.XPRESSCLAW_KEEP_DSH_SMOKE === '1') {
      process.stderr.write(`Retained DeepSeek Harness smoke data at ${root}\n`);
    } else {
      await rm(root, { recursive: true, force: true });
    }
  }
  process.stdout.write('DeepSeek Harness ACP smoke verification passed.\n');
}

main().catch((error) => {
  process.stderr.write(`${error.stack ?? String(error)}\n`);
  process.exitCode = 1;
});
