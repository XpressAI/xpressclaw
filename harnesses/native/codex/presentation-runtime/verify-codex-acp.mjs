import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import readline from 'node:readline';

const adapter = '/usr/local/lib/node_modules/@agentclientprotocol/codex-acp/dist/index.js';
const source = await readFile(adapter, 'utf8');
assert.match(source, /\.join\(root, "\.agents", "skills"\)/);
assert.match(source, /skillsExtraRootsSet\(\{ extraRoots: skillExtraRoots \}\)/);
assert.match(source, /process\.env\["CODEX_CONFIG"\]/);

const child = spawn('codex-acp', [], { stdio: ['pipe', 'pipe', 'pipe'] });
const lines = readline.createInterface({ input: child.stdout });
const timeout = setTimeout(() => child.kill('SIGKILL'), 10_000);
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
  assert.equal(response?.result?.agentInfo?.version, '1.1.7');
  assert.deepEqual(
    response?.result?.agentCapabilities?.sessionCapabilities?.additionalDirectories,
    {},
  );
} finally {
  clearTimeout(timeout);
  child.stdin.end();
  child.kill('SIGTERM');
  lines.close();
}
