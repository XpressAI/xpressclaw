#!/usr/bin/env node

import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import { pathToFileURL } from 'node:url';

async function waitForClose(closed, timeoutMs) {
  let timer;
  try {
    return await Promise.race([
      closed.then(() => true),
      new Promise((resolve) => { timer = setTimeout(() => resolve(false), timeoutMs); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

export async function verifyAcpInitialize(command, {
  timeoutMs = 60_000,
  shutdownTimeoutMs = 5_000,
  log = console.error,
} = {}) {
  assert.ok(Array.isArray(command) && command.length > 0, 'ACP command must be a non-empty JSON array');
  assert.ok(command.every((part) => typeof part === 'string' && part.length > 0), 'ACP command arguments must be non-empty strings');
  assert.notEqual(process.platform, 'win32', 'ACP runner smoke tests require POSIX process groups');
  for (const duration of [timeoutMs, shutdownTimeoutMs]) {
    assert.ok(Number.isSafeInteger(duration) && duration > 0 && duration <= 2 ** 31 - 1, 'Smoke-test deadlines must be positive timer durations');
  }

  log(`ACP smoke: initializing ${JSON.stringify(command)} on ${process.arch} (deadline ${timeoutMs}ms)`);
  const child = spawn(command[0], command.slice(1), {
    env: { ...process.env, NO_BROWSER: '1' },
    // Launchers such as Qwen spawn another process with inherited stdio. Give
    // the whole tree its own group so killing the launcher cannot orphan it.
    detached: true,
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  const closed = new Promise((resolve) => child.once('close', resolve));
  const lines = readline.createInterface({ input: child.stdout });
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => {
    stderr = `${stderr}${chunk}`.slice(-16 * 1024);
  });
  const signalGroup = (signal) => {
    if (!child.pid) return;
    try {
      process.kill(-child.pid, signal);
    } catch (error) {
      if (error.code !== 'ESRCH') throw error;
    }
  };

  let timer;
  let failure;
  try {
    const response = await new Promise((resolve, reject) => {
      // Reject the wait itself. Killing a PID alone does not close pipes that
      // are still held by grandchildren, so readline can otherwise wait forever.
      timer = setTimeout(() => reject(new Error(`ACP initialize timed out after ${timeoutMs}ms`)), timeoutMs);
      child.once('error', reject);
      child.stdin.on('error', reject);
      child.stdout.on('error', reject);
      child.stderr.on('error', reject);
      lines.once('close', () => reject(new Error('ACP server closed stdout without answering initialize')));
      lines.on('line', (line) => {
        try {
          const candidate = JSON.parse(line);
          if (candidate?.id === 1) resolve(candidate);
        } catch (error) {
          reject(new Error(`Invalid ACP JSON: ${error.message}`));
        }
      });
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
    });
    assert.equal(response.error, undefined, `ACP initialize failed: ${JSON.stringify(response.error)}`);
    assert.ok(response.result && typeof response.result === 'object', 'ACP initialize returned no result object');
    log('ACP smoke: initialize succeeded');
  } catch (error) {
    failure = error;
  } finally {
    clearTimeout(timer);
    log('ACP smoke: stopping process group');
    try {
      child.stdin.end();
      signalGroup('SIGTERM');
      if (!await waitForClose(closed, shutdownTimeoutMs)) {
        log(`ACP smoke: shutdown grace expired after ${shutdownTimeoutMs}ms; sending SIGKILL`);
      }
      // Even a closed launcher can leave descendants that no longer share its
      // pipes. Always terminate the remaining group, not just an open child.
      signalGroup('SIGKILL');
      if (!await waitForClose(closed, shutdownTimeoutMs)) {
        throw new Error(`ACP process pipes did not close after SIGKILL within ${shutdownTimeoutMs}ms`);
      }
    } catch (error) {
      failure = failure ? new Error(`${failure.message}\nCleanup failed: ${error.message}`) : error;
    } finally {
      lines.close();
      child.stdin.destroy();
      child.stdout.destroy();
      child.stderr.destroy();
      child.unref();
    }
  }

  if (failure) throw new Error(`${failure.message}${stderr ? `\nACP stderr (tail):\n${stderr}` : ''}`);
  log('ACP smoke: process group stopped');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const encodedCommand = process.argv[2] ?? '';
    const command = JSON.parse(Buffer.from(encodedCommand, 'base64').toString('utf8') || 'null');
    await verifyAcpInitialize(command);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
