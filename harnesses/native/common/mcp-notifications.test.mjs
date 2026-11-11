import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

for (const [server, tool] of [['mcp-github.mjs', 'gh'], ['mcp-xpressclaw.mjs', 'list_project_tasks']]) {
  test(`${server} keeps tool discovery usable after MCP notifications`, () => {
    const messages = [
      { id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25' } },
      { method: 'notifications/initialized' },
      { method: 'notifications/roots/list_changed' },
      { method: 'notifications/example-extension' },
      { id: 0, method: 'ping' },
      { id: 2, method: 'tools/list' },
    ];
    const child = spawnSync(process.execPath, [fileURLToPath(new URL(server, import.meta.url))], {
      input: messages.map((message) => JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n').join(''),
      encoding: 'utf8', timeout: 5000,
    });
    assert.equal(child.status, 0, child.stderr || child.error?.message);
    const replies = child.stdout.trim().split('\n').map((line) => JSON.parse(line));
    assert.deepEqual(replies.map((reply) => reply.id).sort(), [0, 1, 2]);
    assert.ok(replies.every((reply) => !reply.error));
    assert.ok(replies.find((reply) => reply.id === 2).result.tools.some((entry) => entry.name === tool));
  });
}
