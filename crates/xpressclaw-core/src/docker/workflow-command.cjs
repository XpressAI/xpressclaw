const { spawn } = require('node:child_process');
const { realpathSync } = require('node:fs');
const { resolve, relative, isAbsolute } = require('node:path');

const LIMIT = 1024 * 1024;
let input = '';
let started = false;
let child;
let finished = false;
let timer;
let timedOut = false;
function stop() {
  if (child?.pid) {
    try { process.kill(-child.pid, 'SIGKILL'); } catch (error) {
      if (error.code !== 'ESRCH') throw error;
    }
  }
}
function reply(value) {
  if (finished) return;
  finished = true;
  clearTimeout(timer);
  stop();
  process.stdout.write(JSON.stringify(value), () => process.exit(0));
}
process.stdin.setEncoding('utf8');
process.stdin.on('end', () => { stop(); process.exit(1); });
process.on('SIGTERM', () => { stop(); process.exit(1); });
process.on('SIGINT', () => { stop(); process.exit(1); });
process.stdin.on('data', (chunk) => {
  if (started) return;
  input += chunk;
  if (input.length > LIMIT) return reply({ error: 'Command request exceeds 1 MiB' });
  const newline = input.indexOf('\n');
  if (newline < 0) return;
  started = true;
  try {
    const request = JSON.parse(input.slice(0, newline));
    if (typeof request.script !== 'string' || !request.script.trim()) throw new Error('Command is empty');
    if (!Number.isInteger(request.timeout_secs) || request.timeout_secs < 1 || request.timeout_secs > 86400) throw new Error('Command timeout must be between 1s and 24h');
    const root = realpathSync(process.cwd());
    const cwd = realpathSync(resolve(root, request.cwd || '.'));
    const rel = relative(root, cwd);
    if (rel === '..' || rel.startsWith('../') || isAbsolute(rel)) throw new Error('Command directory is outside the Agent workspace');
    const buffers = { stdout: [], stderr: [] };
    const sizes = { stdout: 0, stderr: 0 };
    const truncated = { stdout: false, stderr: false };
    child = spawn('/bin/sh', ['-c', request.script], { cwd, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
    for (const stream of ['stdout', 'stderr']) {
      child[stream].on('data', (chunk) => {
        const remaining = LIMIT - sizes[stream];
        if (chunk.length > remaining) truncated[stream] = true;
        if (remaining > 0) {
          const data = chunk.subarray(0, remaining);
          buffers[stream].push(data);
          sizes[stream] += data.length;
        }
      });
    }
    timer = setTimeout(() => { timedOut = true; stop(); }, request.timeout_secs * 1000);
    child.on('error', (error) => reply({ error: error.message }));
    // A shell can leave background children holding its pipes open. The
    // workflow command owns that process group until all output is collected.
    child.on('exit', stop);
    child.on('close', (code, signal) => reply({
      exit_code: code, signal, timed_out: timedOut,
      stdout: Buffer.concat(buffers.stdout).toString('utf8'),
      stderr: Buffer.concat(buffers.stderr).toString('utf8'),
      stdout_truncated: truncated.stdout, stderr_truncated: truncated.stderr,
    }));
  } catch (error) { reply({ error: error.message }); }
});
