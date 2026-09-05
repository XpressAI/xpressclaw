#!/usr/bin/env node

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import readline from 'node:readline';

const encodedCommand = process.argv[2] ?? '';
const command = JSON.parse(Buffer.from(encodedCommand, 'base64').toString('utf8') || 'null');
assert.ok(Array.isArray(command) && command.length > 0, 'ACP command must be a non-empty JSON array');
assert.ok(command.every((part) => typeof part === 'string' && part.length > 0), 'ACP command arguments must be non-empty strings');

const child = spawn(command[0], command.slice(1), {
  env: { ...process.env, NO_BROWSER: '1' },
  stdio: ['pipe', 'pipe', 'pipe'],
});
const lines = readline.createInterface({ input: child.stdout });
let stderr = '';
child.stderr.setEncoding('utf8');
child.stderr.on('data', (chunk) => {
  stderr = `${stderr}${chunk}`.slice(-16 * 1024);
});

const timeout = setTimeout(() => child.kill('SIGKILL'), 60_000);
try {
  child.stdin.write(`${JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'initialize',
    params: {
      protocolVersion: 1,
      clientCapabilities: {
        fs: { readTextFile: true, writeTextFile: true },
        terminal: true,
      },
      clientInfo: {
        name: 'xpressclaw-runner-smoke',
        title: 'XpressClaw runner smoke',
        version: '1',
      },
    },
  })}\n`);

  let response;
  for await (const line of lines) {
    const candidate = JSON.parse(line);
    if (candidate.id === 1) {
      response = candidate;
      break;
    }
  }
  assert.ok(response, `ACP server exited without answering initialize\n${stderr}`);
  assert.equal(response.error, undefined, `ACP initialize failed: ${JSON.stringify(response.error)}\n${stderr}`);
  assert.ok(response.result && typeof response.result === 'object', 'ACP initialize returned no result object');
} finally {
  clearTimeout(timeout);
  child.stdin.end();
  child.kill('SIGTERM');
  lines.close();
}
