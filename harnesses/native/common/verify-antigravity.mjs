#!/usr/bin/env node

// Exercise the official CLI against local Gemini and MCP fixtures.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const binary = process.argv[2] ?? 'agy';
const common = dirname(fileURLToPath(import.meta.url));
const root = await mkdtemp(join(tmpdir(), 'xpressclaw-antigravity-'));
const home = join(root, 'home');
const cwd = join(root, 'workspace');
await Promise.all([mkdir(home), mkdir(cwd)]);
let processClient;
let fixtureError;
let modelStep = 0;
let scopedCalls = 0;
let httpCalls = 0;
let cancellationStarted;
const cancelling = new Promise((done) => { cancellationStarted = done; });

function stream(response, parts) {
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  response.end(`data: ${JSON.stringify({
    candidates: [{ content: { role: 'model', parts }, finishReason: 'STOP', index: 0 }],
    usageMetadata: { promptTokenCount: 10, candidatesTokenCount: 5, totalTokenCount: 15 },
  })}\n\n`);
}

const server = createServer(async (request, response) => {
  try {
    let text = '';
    for await (const chunk of request) text += chunk;
    const body = JSON.parse(text || '{}');
    const url = new URL(request.url, 'http://127.0.0.1');
    if (url.pathname.startsWith('/api/environments/')) {
      assert.equal(url.pathname, '/api/environments/smoke-agent/tasks');
      assert.equal(request.headers['x-xpressclaw-agent-id'], 'smoke-agent');
      scopedCalls++;
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end('{"tasks":[]}');
    } else if (url.pathname === '/mcp') {
      assert.equal(request.headers['x-smoke-scope'], 'agent');
      if (body.id === undefined) { response.writeHead(204); response.end(); return; }
      let result;
      if (body.method === 'initialize') {
        result = { protocolVersion: '2025-03-26', capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' } };
      } else if (body.method === 'tools/list') {
        result = { tools: [{ name: 'network_probe', description: 'Check HTTP MCP', inputSchema: { type: 'object', properties: {} } }] };
      } else if (body.method === 'tools/call') {
        assert.equal(body.params.name, 'network_probe');
        httpCalls++;
        result = { content: [{ type: 'text', text: 'http-ok' }] };
      } else {
        response.writeHead(200, { 'content-type': 'application/json' });
        response.end(JSON.stringify({ jsonrpc: '2.0', id: body.id, error: { code: -32601, message: 'Method not found' } }));
        return;
      }
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ jsonrpc: '2.0', id: body.id, result }));
    } else if (url.pathname.includes(':streamGenerateContent')) {
      const tools = body.tools?.flatMap((group) => group.functionDeclarations ?? []) ?? [];
      // Background title requests have no tools and do not advance the test turn.
      if (!tools.length) { stream(response, [{ text: 'Smoke session' }]); return; }
      if (JSON.stringify(body.contents).includes('wait-for-cancellation')) {
        response.writeHead(200, { 'content-type': 'text/event-stream' });
        response.write(': waiting\n\n');
        cancellationStarted();
        return;
      }
      for (const name of ['mcp_github_gh', 'mcp_xpressclaw_list_project_tasks', 'mcp_http-fixture_network_probe']) {
        assert.ok(tools.some((tool) => tool.name === name), `Missing injected tool: ${name}`);
      }
      const calls = [
        { name: 'mcp_xpressclaw_list_project_tasks', args: { toolSummary: 'List project tasks' } },
        { name: 'mcp_http-fixture_network_probe', args: { toolSummary: 'Check HTTP MCP' } },
        { name: 'run_command', args: { CommandLine: "printf 'permission-ok' > command-result", toolSummary: 'Write smoke fixture', WaitMsBeforeAsync: 1000 } },
      ];
      const call = calls[modelStep++];
      stream(response, [call ? { functionCall: call } : { text: 'antigravity-smoke-ok' }]);
    } else {
      response.writeHead(404); response.end();
    }
  } catch (error) {
    fixtureError = error;
    response.writeHead(500); response.end('fixture failed');
  }
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const base = `http://127.0.0.1:${server.address().port}`;

function start() {
  const child = spawn(binary, ['acp', '--process-per-session', '--disable-telemetry'], {
    cwd, detached: true, stdio: ['pipe', 'pipe', 'pipe'],
    env: { ...process.env, HOME: home, AGY_ADC_AUTH: 'false', GEMINI_API_KEY: 'xpressclaw-smoke-not-a-real-key', GOOGLE_GEMINI_BASE_URL: base },
  });
  const exited = new Promise((done) => child.once('close', done));
  const lines = createInterface({ input: child.stdout });
  const pending = new Map();
  const updates = [];
  let permissions = 0;
  let stderr = '';
  let nextId = 0;
  child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-8000); });
  const failed = (error) => { for (const request of pending.values()) request.reject(error); pending.clear(); };
  child.on('error', failed);
  child.stdin.on('error', failed);
  lines.on('close', () => failed(new Error(`ACP exited before responding: ${stderr}`)));
  lines.on('line', (line) => {
    try {
      const message = JSON.parse(line);
      if (message.method === 'session/request_permission') {
        const option = message.params.options.find((option) => option.kind === 'allow_once');
        assert.ok(option, 'No allow-once permission option');
        permissions++;
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: message.id, result: { outcome: { outcome: 'selected', optionId: option.optionId } } }) + '\n');
      } else if (message.method === 'session/update') updates.push(message.params.update);
      else if (pending.has(message.id)) {
        const request = pending.get(message.id); pending.delete(message.id);
        if (message.error) request.reject(Object.assign(new Error(message.error.message), { code: message.error.code }));
        else request.resolve(message.result);
      }
    } catch (error) { failed(error); }
  });
  return {
    updates,
    get permissions() { return permissions; },
    notify(method, params) { child.stdin.write(JSON.stringify({ jsonrpc: '2.0', method, params }) + '\n'); },
    async request(method, params) {
      const id = ++nextId;
      let timer;
      try {
        return await Promise.race([
          new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n'); }),
          new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${method} timed out: ${stderr}`)), 30_000); }),
        ]);
      } finally { clearTimeout(timer); pending.delete(id); }
    },
    async stop() {
      child.stdin.end();
      const signal = (name) => { if (!child.pid) return; try { process.kill(-child.pid, name); } catch (error) { if (error.code !== 'ESRCH') throw error; } };
      signal('SIGTERM');
      let timer;
      await Promise.race([exited, new Promise((done) => { timer = setTimeout(done, 2000); })]);
      clearTimeout(timer);
      signal('SIGKILL');
      lines.close(); child.stdin.destroy(); child.stdout.destroy(); child.stderr.destroy(); child.unref();
    },
  };
}

const initialize = () => processClient.request('initialize', { protocolVersion: 1, clientInfo: { name: 'xpressclaw', version: 'smoke' }, clientCapabilities: {} });
const mcpServers = [
  { name: 'github', command: process.execPath, args: [join(common, 'mcp-github.mjs')], env: [] },
  { name: 'xpressclaw', command: process.execPath, args: [join(common, 'mcp-xpressclaw.mjs')], env: [
    { name: 'XPRESSCLAW_URL', value: base }, { name: 'XPRESSCLAW_AGENT_ID', value: 'smoke-agent' },
  ] },
  { type: 'http', name: 'http-fixture', url: `${base}/mcp`, headers: [{ name: 'X-Smoke-Scope', value: 'agent' }] },
];

try {
  processClient = start();
  const initialized = await initialize();
  assert.equal(initialized.protocolVersion, 1);
  assert.ok(initialized.agentCapabilities.loadSession && initialized.agentCapabilities.mcpCapabilities.http);
  assert.ok(initialized.authMethods.some((method) => method.id === 'gemini-api-key'));
  await assert.rejects(processClient.request('session/new', { cwd, mcpServers: [] }), { code: -32000 });
  await processClient.stop();
  await mkdir(join(home, '.gemini/antigravity-cli'), { recursive: true });
  await writeFile(join(home, '.gemini/antigravity-cli/settings.json'), '{"modelProvider":"gemini"}');
  processClient = start();
  await initialize();
  const session = await processClient.request('session/new', { cwd, mcpServers });
  const sessionId = session.sessionId;
  for (const category of ['model', 'thought_level']) assert.ok(session.configOptions.some((option) => option.category === category));
  const thought = session.configOptions.find((option) => option.category === 'thought_level');
  await processClient.request('session/set_config_option', { sessionId, configId: thought.id, value: thought.options.at(-1).value });
  const prompted = await processClient.request('session/prompt', { sessionId, prompt: [{ type: 'text', text: 'Exercise the smoke tools and reply.' }] });
  assert.equal(prompted.stopReason, 'end_turn');
  assert.equal(await readFile(join(cwd, 'command-result'), 'utf8'), 'permission-ok');
  assert.ok(processClient.permissions > 0);
  assert.ok(processClient.updates.some((update) => update.sessionUpdate === 'tool_call'));
  assert.ok(processClient.updates.some((update) => update.content?.text === 'antigravity-smoke-ok'));
  assert.equal(scopedCalls, 1, JSON.stringify(processClient.updates.filter(x => x.sessionUpdate.startsWith('tool_call')))); assert.equal(httpCalls, 1);
  await processClient.request('session/close', { sessionId });
  await processClient.stop();
  processClient = start();
  await initialize();
  await processClient.request('session/load', { sessionId, cwd, mcpServers });
  assert.ok(processClient.updates.some((update) => update.content?.text === 'antigravity-smoke-ok'), 'Saved session was not reloaded after process recreation');
  await processClient.request('session/close', { sessionId });
  await processClient.request('session/resume', { sessionId, cwd, mcpServers });
  const fresh = await processClient.request('session/new', { cwd, mcpServers });
  const cancelled = processClient.request('session/prompt', { sessionId: fresh.sessionId, prompt: [{ type: 'text', text: 'wait-for-cancellation' }] });
  let timer;
  try { await Promise.race([cancelling, cancelled.then(() => { throw new Error('Prompt ended before cancellation'); }), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Cancellation stream did not start')), 20_000); })]); }
  finally { clearTimeout(timer); }
  processClient.notify('session/cancel', { sessionId: fresh.sessionId });
  assert.equal((await cancelled).stopReason, 'cancelled');
  if (fixtureError) throw fixtureError;
  console.error('Antigravity smoke passed: provider selection, model controls, scoped stdio/HTTP MCP, permissions, tools, saved sessions, cancellation.');
} finally {
  await processClient?.stop();
  server.closeAllConnections(); server.close();
  await rm(root, { recursive: true, force: true });
}
