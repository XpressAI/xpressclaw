import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, writeFile, symlink, mkdir, stat, realpath, truncate } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createServer, createConnection } from 'node:net';
import { createInterface } from 'node:readline';

const filesScript = await readFile(new URL('../../../crates/xpressclaw-core/src/docker/files.cjs', import.meta.url), 'utf8');
const bridgeScript = await readFile(new URL('../../../crates/xpressclaw-core/src/docker/bridge.cjs', import.meta.url), 'utf8');
async function files(request) {
  const child = spawn(process.execPath, ['-e', filesScript], { stdio: ['pipe', 'pipe', 'pipe'] });
  const chunks = [];
  child.stdout.on('data', chunk => chunks.push(chunk));
  const exited = once(child, 'exit');
  child.stdin.write(JSON.stringify(request) + '\n');
  const [code] = await exited;
  assert.equal(code, 0);
  return JSON.parse(Buffer.concat(chunks).toString());
}

test('uploads stay private and outside Git even without ignore rules', { timeout: 10000 }, async () => {
  const workspace = await mkdtemp(path.join(tmpdir(), 'xpressclaw-upload-repository-'));
  const stagedDirectories = [];
  try {
    execFileSync('git', ['init', '-q', workspace]);
    const data = Buffer.alloc(6 * 1024 * 1024, 255);
    const staged = await files({ operation: 'stage', workspace, name: '資料'.repeat(100) + '.bin', data: data.toString('base64') });
    assert.ifError(staged.error);
    stagedDirectories.push(path.dirname(staged.path));
    assert.ok(!staged.path.startsWith((await realpath(workspace)) + path.sep));
    assert.ok(Buffer.byteLength(path.basename(staged.path)) <= 180);
    assert.deepEqual(await readFile(staged.path), data);
    assert.equal((await stat(staged.path)).mode & 0o777, 0o600);
    assert.equal((await stat(path.dirname(staged.path))).mode & 0o777, 0o700);
    execFileSync('git', ['-C', workspace, 'add', '-A']);
    assert.equal(execFileSync('git', ['-C', workspace, 'status', '--porcelain'], { encoding: 'utf8' }), '');
    assert.equal(execFileSync('git', ['-C', workspace, 'ls-files'], { encoding: 'utf8' }), '');

    const fallback = await files({ operation: 'stage', workspace: '/var/tmp', name: '../input.txt', data: 'aGVsbG8=' });
    assert.ifError(fallback.error);
    stagedDirectories.push(path.dirname(fallback.path));
    assert.ok(!fallback.path.startsWith((await realpath('/var/tmp')) + path.sep));
    assert.equal(await readFile(fallback.path, 'utf8'), 'hello');
    assert.match((await files({ operation: 'stage', workspace: '/', name: 'input', data: '' })).error, /outside/);
  } finally {
    for (const directory of stagedDirectories) await rm(directory, { recursive: true, force: true });
    await rm(workspace, { recursive: true, force: true });
  }
});

test('container files support binary downloads, archives, UTF-8 edits and revision conflicts', { timeout: 10000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'xpressclaw-files-'));
  try {
    const file = path.join(directory, 'résumé.txt');
    await writeFile(file, 'original 日本語');
    await writeFile(path.join(directory, 'binary.bin'), Buffer.from([0, 255, 1]));
    await mkdir(path.join(directory, 'nested'));
    await symlink('/proc', path.join(directory, 'kernel-link'));
    const tree = await files({ operation: 'tree', path: directory });
    assert.equal(tree.entries.find(entry => entry.name === 'kernel-link').kind, 'symlink');
    const opened = await files({ operation: 'read', path: file });
    assert.equal(opened.content, 'original 日本語');
    await writeFile(file, 'changed externally');
    assert.match((await files({ operation: 'write', path: file, content: 'overwrite', expected_revision: opened.revision })).error, /changed/);
    const current = await files({ operation: 'read', path: file });
    const saved = await files({ operation: 'write', path: file, content: 'new content', expected_revision: current.revision });
    assert.equal(saved.content, 'new content');
    assert.equal(await readFile(file, 'utf8'), 'new content');
    assert.ok((await files({ operation: 'read', path: path.join(directory, 'binary.bin') })).error);
    const binary = await files({ operation: 'download', path: path.join(directory, 'binary.bin') });
    assert.deepEqual(Buffer.from(binary.data, 'base64'), Buffer.from([0, 255, 1]));
    assert.match((await files({ operation: 'tree', path: path.join(directory, 'kernel-link') })).error, /kernel/);
    const archive = await files({ operation: 'download', path: directory });
    assert.equal(archive.name, path.basename(directory) + '.tar.gz');
    assert.equal(archive.mime_type, 'application/gzip');
    assert.deepEqual(Buffer.from(archive.data, 'base64').subarray(0, 2), Buffer.from([0x1f, 0x8b]));
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('bridge accepts a loopback connection, transfers binary data and closes its listener on stdin EOF', { timeout: 10000 }, async () => {
  const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
  const port = reservation.address().port;
  await new Promise(resolve => reservation.close(resolve));
  const child = spawn(process.execPath, ['-e', bridgeScript, 'host_to_container', String(port)]);
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  try {
    assert.equal(JSON.parse((await lines.next()).value).type, 'ready');
    const socket = createConnection({ host: '127.0.0.1', port, allowHalfOpen: true });
    await once(socket, 'connect');
    const opened = JSON.parse((await lines.next()).value); assert.equal(opened.type, 'open');
    const received = []; socket.on('data', chunk => received.push(chunk));
    const ended = once(socket, 'end');
    child.stdin.write(JSON.stringify({ type: 'data', id: opened.id, data: Buffer.from([0, 1, 255]).toString('base64') }) + '\n');
    child.stdin.write(JSON.stringify({ type: 'end', id: opened.id }) + '\n');
    await ended; assert.deepEqual(Buffer.concat(received), Buffer.from([0, 1, 255]));
    socket.end('response');
    let response = Buffer.alloc(0);
    for await (const line of { [Symbol.asyncIterator]: () => lines }) {
      const frame = JSON.parse(line);
      if (frame.type === 'data') response = Buffer.concat([response, Buffer.from(frame.data, 'base64')]);
      if (frame.type === 'end') break;
    }
    assert.equal(response.toString(), 'response');
    const exited = once(child, 'exit'); child.stdin.end(); await exited;
    const reused = createServer(); reused.listen(port, '127.0.0.1'); await once(reused, 'listening'); await new Promise(resolve => reused.close(resolve));
  } finally { child.kill(); }
});

test('file size preflight and bounded downloads reject oversized files and folders', { timeout: 10000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'xpressclaw-file-budget-'));
  try {
    const oversized = path.join(directory, 'oversized.bin');
    await writeFile(oversized, '');
    await truncate(oversized, 21 * 1024 * 1024);
    for (const filename of [oversized, directory]) {
      const size = await files({ operation: 'stat', path: filename, max_bytes: 20 * 1024 * 1024 });
      assert.match(size.error, /download budget/);
      const download = await files({ operation: 'download', path: filename, max_bytes: 20 * 1024 * 1024 });
      assert.ok(download.error);
      assert.equal(download.data, undefined);
    }
    const links = path.join(directory, 'links');
    await mkdir(links);
    await symlink(oversized, path.join(links, 'large-link'));
    const info = await files({ operation: 'stat', path: links, max_bytes: 1024 });
    assert.ifError(info.error);
    assert.ok(info.size < 1024, 'preflight must not follow archive symlinks');
    await truncate(oversized, 101 * 1024 * 1024);
    assert.match((await files({ operation: 'download', path: oversized })).error, /100 MiB/);
    assert.match((await files({ operation: 'download', path: oversized, max_bytes: 101 * 1024 * 1024 })).error, /between 0 and 100 MiB/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test('blackholed container connections time out and release all socket slots', { timeout: 5000 }, async () => {
  // Inject sockets that never emit connect, and accelerate only the production
  // connection deadline. No live network blackhole or platform firewall needed.
  const injection = `{
    const net = require('node:net');
    const { Duplex } = require('node:stream');
    net.createConnection = () => new Duplex({ read() {}, write(data, encoding, done) { done(); } });
    const timeout = global.setTimeout;
    global.setTimeout = (callback, delay) => timeout(callback, delay === 10000 ? 25 : delay);
  }\n`;
  const child = spawn(process.execPath, ['-e', injection + bridgeScript, 'container_to_host', '3000']);
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  try {
    assert.equal(JSON.parse((await lines.next()).value).type, 'ready');
    for (let id = 1; id <= 64; id++) child.stdin.write(JSON.stringify({ type: 'open', id }) + '\n');
    const closed = new Set();
    for (let count = 0; count < 64; count++) {
      const frame = JSON.parse((await lines.next()).value);
      assert.equal(frame.type, 'close'); closed.add(frame.id);
    }
    assert.equal(closed.size, 64);
    child.stdin.write(JSON.stringify({ type: 'open', id: 1 }) + '\n');
    assert.deepEqual(JSON.parse((await lines.next()).value), { type: 'close', id: 1 });
  } finally { child.kill(); }
});

test('successful container connections clear the connect deadline', { timeout: 5000 }, async () => {
  const server = createServer(socket => socket.pipe(socket));
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const injection = `{ const timeout = global.setTimeout; global.setTimeout = (callback, delay) => timeout(callback, delay === 10000 ? 100 : delay); }\n`;
  const child = spawn(process.execPath, ['-e', injection + bridgeScript, 'container_to_host', String(server.address().port)]);
  const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
  try {
    assert.equal(JSON.parse((await lines.next()).value).type, 'ready');
    const accepted = once(server, 'connection');
    child.stdin.write(JSON.stringify({ type: 'open', id: 1 }) + '\n');
    await accepted;
    await new Promise(resolve => setTimeout(resolve, 200));
    child.stdin.write(JSON.stringify({ type: 'data', id: 1, data: 'aGVsbG8=' }) + '\n');
    assert.deepEqual(JSON.parse((await lines.next()).value), { type: 'data', id: 1, data: 'aGVsbG8=' });
  } finally {
    const exited = once(child, 'exit'); child.kill(); await exited;
    await new Promise(resolve => server.close(resolve));
  }
});

test('bridge reports an occupied container port before readiness', { timeout: 5000 }, async () => {
  const server = createServer(); server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const child = spawn(process.execPath, ['-e', bridgeScript, 'host_to_container', String(server.address().port)]);
  try {
    const lines = createInterface({ input: child.stdout });
    const [line] = await once(lines, 'line');
    const frame = JSON.parse(line);
    assert.equal(frame.type, 'error'); assert.match(frame.message, /EADDRINUSE/);
  } finally { child.kill(); await new Promise(resolve => server.close(resolve)); }
});
