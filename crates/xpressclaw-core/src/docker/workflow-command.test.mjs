import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, symlinkSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
const runner = fileURLToPath(new URL('./workflow-command.cjs', import.meta.url));

function run(script, options = {}) {
  const child = spawn(process.execPath, [runner], { cwd: options.root, stdio: ['pipe', 'pipe', 'pipe'] });
  const done = new Promise((resolve, reject) => {
    let output = '', error = '';
    child.stdout.on('data', data => { output += data; });
    child.stderr.on('data', data => { error += data; });
    child.on('error', reject);
    child.on('close', (code) => {
      if (options.disconnect) return resolve({ code });
      try { resolve(JSON.parse(output)); } catch { reject(new Error(`No result: ${code}: ${error}`)); }
    });
  });
  child.stdin.write(JSON.stringify({ script, cwd: options.cwd || '.', workspace_root: options.workspaceRoot, timeout_secs: options.timeout || 5 }) + '\n');
  if (options.disconnect) setTimeout(() => child.stdin.end(), 150);
  return done;
}
const linux = process.platform === 'linux';
test('captures separate stdout/stderr and the actual nonzero exit', { skip: !linux }, async () => {
  const result = await run("printf 'hello'; printf 'failure' >&2; exit 7");
  assert.equal(result.exit_code, 7);
  assert.equal(result.stdout, 'hello');
  assert.equal(result.stderr, 'failure');
  assert.equal(result.timed_out, false);
});
test('runs scripts literally, including newlines and shell arguments', { skip: !linux }, async () => {
  const result = await run("printf '%s\\n' '$(not-expanded)'\nprintf '%s' 'second line'");
  assert.equal(result.stdout, '$(not-expanded)\nsecond line');
  assert.equal(result.exit_code, 0);
});
test('caps captured output without blocking a noisy process', { skip: !linux }, async () => {
  const result = await run("head -c 1100000 /dev/zero; head -c 1100000 /dev/zero >&2");
  assert.equal(result.stdout.length, 1024 * 1024);
  assert.equal(result.stderr.length, 1024 * 1024);
  assert.equal(result.stdout_truncated, true);
  assert.equal(result.stderr_truncated, true);
  assert.equal(result.exit_code, 0);
});
test('timeout kills a process group and records failure', { skip: !linux }, async () => {
  const start = Date.now();
  const result = await run('sleep 30 & wait', { timeout: 1 });
  assert.equal(result.timed_out, true);
  assert.equal(result.exit_code, null);
  assert.equal(result.signal, 'SIGKILL');
  assert.ok(Date.now() - start < 5000);
});
test('cleans background processes when the command exits', { skip: !linux }, async () => {
  const start = Date.now();
  const result = await run('sleep 30 & printf done');
  assert.equal(result.exit_code, 0);
  assert.equal(result.stdout, 'done');
  assert.ok(Date.now() - start < 3000);
});
test('working directories stay inside the real workspace boundary', { skip: !linux }, async () => {
  const root = mkdtempSync(join(tmpdir(), 'workflow-command-'));
  try {
    mkdirSync(join(root, 'tests'));
    symlinkSync(tmpdir(), join(root, 'escape'));
    const inside = await run('pwd', { root, cwd: 'tests' });
    assert.equal(inside.stdout.trim(), join(root, 'tests'));
    assert.match((await run('pwd', { root, cwd: '../' })).error, /outside/);
    assert.match((await run('pwd', { root, cwd: 'escape' })).error, /outside/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
test('absolute candidate paths remain usable when the active repository changes', { skip: !linux }, async () => {
  const root = mkdtempSync(join(tmpdir(), 'workflow-candidates-'));
  try {
    const first = join(root, 'candidate-a');
    const second = join(root, 'candidate-b');
    mkdirSync(first); mkdirSync(second);
    const selected = await run('pwd', { root: second, workspaceRoot: root, cwd: first });
    assert.equal(selected.exit_code, 0);
    assert.equal(selected.stdout.trim(), first);
    const relative = await run('pwd', { root: second, workspaceRoot: root });
    assert.equal(relative.stdout.trim(), second);
    assert.match((await run('pwd', { root: second, workspaceRoot: root, cwd: tmpdir() })).error, /outside/);
    symlinkSync(tmpdir(), join(root, 'escape'));
    assert.match((await run('pwd', { root: second, workspaceRoot: root, cwd: join(root, 'escape') })).error, /outside/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
test('disconnect terminates the process group before it can write later', { skip: !linux }, async () => {
  const root = mkdtempSync(join(tmpdir(), 'workflow-disconnect-'));
  try {
    const result = await run('(sleep 1; printf alive > marker) & wait', { root, disconnect: true });
    assert.equal(result.code, 1);
    await new Promise(resolve => setTimeout(resolve, 1100));
    assert.equal(existsSync(join(root, 'marker')), false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
test('rejects malformed commands and unbounded timeouts', { skip: !linux }, async () => {
  assert.match((await run('')).error, /empty/);
  assert.match((await run('true', { timeout: 86401 })).error, /timeout/);
});
