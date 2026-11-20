import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import test from 'node:test';

import { verifyAcpInitialize } from './verify-acp-initialize.mjs';

const options = { timeout: 5_000, skip: process.platform === 'win32' };
const fixtureSource = `
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';

const [mode, directory] = process.argv.slice(2);
const grandchild = mode.startsWith('grandchild-');
if (grandchild || mode === 'eof') {
  process.on('SIGTERM', () => {});
  setInterval(() => {}, 1_000);
}
writeFileSync(join(directory, grandchild ? 'grandchild.pid' : 'launcher.pid'), String(process.pid));
if (mode.startsWith('wrapper-')) {
  // Qwen's npm launcher uses spawnSync with inherited stdio. Killing just
  // this launcher leaves the grandchild holding the verifier's pipes open.
  const result = spawnSync(process.execPath, [fileURLToPath(import.meta.url), mode.replace('wrapper-', 'grandchild-'), directory], { stdio: 'inherit' });
  process.exit(result.status ?? 1);
}
if (mode === 'exit') {
  process.stderr.write('fixture startup failed\\n');
  process.exit(7);
}
if (mode === 'unshared-pipes') {
  spawn(process.execPath, [fileURLToPath(import.meta.url), 'grandchild-silent', directory], { stdio: 'ignore' });
  while (!existsSync(join(directory, 'grandchild.pid'))) {
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}
if (mode === 'eof') {
  process.stdout.end();
} else {
  const lines = readline.createInterface({ input: process.stdin });
  lines.on('line', (line) => {
    const request = JSON.parse(line);
    if (mode === 'grandchild-silent') {
      process.stderr.write('fixture still initializing\\n');
      return;
    }
    if (mode === 'malformed') {
      process.stdout.write('not-json\\n');
      return;
    }
    const response = mode === 'rpc-error'
      ? { id: request.id, error: { code: -32603, message: 'fixture failure' } }
      : { id: request.id, result: { protocolVersion: 1 } };
    process.stdout.write(JSON.stringify(response) + '\\n');
  });
}
`;

async function fixture(t, mode) {
  const directory = await mkdtemp(join(tmpdir(), 'xpressclaw-acp-smoke-test-'));
  const script = join(directory, 'agent.mjs');
  await writeFile(script, fixtureSource);
  t.after(async () => {
    // Also clean up if a regression causes the test's outer deadline to fire.
    for (const file of ['launcher.pid', 'grandchild.pid']) {
      try {
        const pid = Number(await readFile(join(directory, file), 'utf8'));
        process.kill(file === 'launcher.pid' ? -pid : pid, 'SIGKILL');
      } catch (error) {
        if (!['ENOENT', 'ESRCH'].includes(error.code)) throw error;
      }
    }
    await rm(directory, { recursive: true, force: true });
  });
  const messages = [];
  return {
    directory,
    messages,
    run: () => verifyAcpInitialize([process.execPath, script, mode, directory], {
      timeoutMs: 1_000,
      shutdownTimeoutMs: 100,
      log: (message) => messages.push(message),
    }),
  };
}

async function running(pid) {
  try {
    process.kill(pid, 0);
    if (process.platform === 'linux') {
      // Container PID 1 may defer reaping an orphan. A zombie has stopped
      // executing and cannot hold pipes open, even though kill(pid, 0) succeeds.
      const stat = await readFile(`/proc/${pid}/stat`, 'utf8');
      return !['Z', 'X'].includes(stat.slice(stat.lastIndexOf(')') + 2, stat.lastIndexOf(')') + 3));
    }
    return true;
  } catch (error) {
    if (['ENOENT', 'ESRCH'].includes(error.code)) return false;
    throw error;
  }
}

async function assertStopped(directory, file) {
  const pid = Number(await readFile(join(directory, file), 'utf8'));
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (!await running(pid)) return;
    await delay(20);
  }
  assert.fail(`${file} process ${pid} survived smoke-test cleanup`);
}

test('initializes an ACP server and stops its process', options, async (t) => {
  const agent = await fixture(t, 'normal');
  await agent.run();
  assert.ok(agent.messages.includes('ACP smoke: initialize succeeded'));
  assert.ok(agent.messages.includes('ACP smoke: process group stopped'));
  await assertStopped(agent.directory, 'launcher.pid');
});

test('initialization deadline kills a wrapper and the grandchild holding its pipes', options, async (t) => {
  const agent = await fixture(t, 'wrapper-silent');
  await assert.rejects(agent.run(), /ACP initialize timed out after 1000ms[\s\S]*fixture still initializing/);
  await assertStopped(agent.directory, 'launcher.pid');
  await assertStopped(agent.directory, 'grandchild.pid');
});

test('successful initialization cannot hang during shutdown when a grandchild ignores SIGTERM', options, async (t) => {
  const agent = await fixture(t, 'wrapper-responsive');
  await agent.run();
  assert.ok(agent.messages.some((message) => message.includes('shutdown grace expired')));
  await assertStopped(agent.directory, 'launcher.pid');
  await assertStopped(agent.directory, 'grandchild.pid');
});

test('cleans up grandchildren even after the launcher and its pipes have closed', options, async (t) => {
  const agent = await fixture(t, 'unshared-pipes');
  await agent.run();
  await assertStopped(agent.directory, 'launcher.pid');
  await assertStopped(agent.directory, 'grandchild.pid');
});

for (const [mode, expected] of [
  ['malformed', /Invalid ACP JSON/],
  ['rpc-error', /ACP initialize failed:[\s\S]*fixture failure/],
  ['exit', /without answering initialize[\s\S]*fixture startup failed/],
  ['eof', /closed stdout without answering initialize/],
]) {
  test(`reports ${mode} initialization failure and cleans up`, options, async (t) => {
    const agent = await fixture(t, mode);
    await assert.rejects(agent.run(), expected);
    await assertStopped(agent.directory, 'launcher.pid');
  });
}

test('reports a missing executable without waiting for the deadline', options, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'xpressclaw-acp-missing-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  await assert.rejects(verifyAcpInitialize([join(directory, 'missing')], {
    timeoutMs: 10_000,
    shutdownTimeoutMs: 100,
    log: () => {},
  }), /ENOENT/);
});
