// Runs inside the Agent container, under its normal filesystem identity.
const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFile } = require('node:child_process');
const { promisify } = require('node:util');
const hash = data => crypto.createHash('sha256').update(data).digest('hex');
const MAX_FILE = 4 * 1024 * 1024;
const MAX_DOWNLOAD = 100 * 1024 * 1024;
function mime(filename) {
  return ({ '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.webp': 'image/webp', '.pdf': 'application/pdf', '.txt': 'text/plain', '.md': 'text/markdown', '.json': 'application/json', '.csv': 'text/csv', '.zip': 'application/zip', '.pptx': 'application/vnd.openxmlformats-officedocument.presentationml.presentation', '.docx': 'application/vnd.openxmlformats-officedocument.wordprocessingml.document', '.xlsx': 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet' })[path.extname(filename).toLowerCase()] ?? 'application/octet-stream';
}
async function main(input) {
  if (input.operation === 'sessions') {
    try {
      const { stdout } = await promisify(execFile)('tmux', ['list-sessions', '-F', '#S'], { maxBuffer: 65536 });
      return { sessions: stdout.trim().split('\n').filter(name => /^[A-Za-z0-9_-]{1,64}$/.test(name)) };
    } catch (error) {
      if (/no server running|error connecting|no sessions/.test(error.stderr ?? '')) return { sessions: [] };
      throw new Error('tmux is unavailable. Rebuild the runner image with tmux installed.');
    }
  }
  const requested = input.path || '/';
  if (!path.isAbsolute(requested) || requested.includes('\0')) throw new Error('An absolute container path is required');
  const resolved = await fs.realpath(requested);
  if (['/proc', '/sys', '/dev'].some(root => resolved === root || resolved.startsWith(root + '/'))) {
    throw new Error('Device and kernel files are not available in the file browser');
  }
  const details = await fs.stat(resolved);
  if (input.operation === 'tree') {
    if (!details.isDirectory()) throw new Error('Not a directory');
    const names = await fs.readdir(resolved, { withFileTypes: true });
    names.sort((a, b) => Number(b.isDirectory()) - Number(a.isDirectory()) || a.name.localeCompare(b.name));
    const entries = await Promise.all(names.slice(0, 2000).map(async entry => {
      const filename = path.join(resolved, entry.name);
      const info = await fs.lstat(filename).catch(() => null);
      return { name: entry.name, path: filename, kind: entry.isDirectory() ? 'directory' : entry.isFile() ? 'file' : entry.isSymbolicLink() ? 'symlink' : 'other', symlink: entry.isSymbolicLink(), size: info?.size ?? null, modified_at: info?.mtime.toISOString() ?? null };
    }));
    return { path: resolved, entries, truncated: names.length > entries.length };
  }
  if (input.operation === 'download' && details.isDirectory()) {
    if (resolved === '/') throw new Error('Choose a folder below the container root');
    const { stdout } = await promisify(execFile)('tar', ['-czf', '-', '-C', path.dirname(resolved), '--', path.basename(resolved)], { encoding: 'buffer', maxBuffer: MAX_DOWNLOAD, timeout: 60000 });
    return { name: path.basename(resolved) + '.tar.gz', mime_type: 'application/gzip', data: stdout.toString('base64') };
  }
  if (!details.isFile()) throw new Error('Not a regular file');
  const limit = input.operation === 'download' ? MAX_DOWNLOAD : MAX_FILE;
  // O_NONBLOCK prevents a raced FIFO from blocking the exec process.
  const file = await fs.open(resolved, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  let data;
  try {
    const current = await file.stat();
    if (!current.isFile() || current.size > limit) throw new Error(`File exceeds ${limit / 1024 / 1024} MiB or is not regular`);
    const buffer = Buffer.alloc(current.size + 1);
    let length = 0;
    while (length < buffer.length) {
      const { bytesRead } = await file.read(buffer, length, buffer.length - length, null);
      if (!bytesRead) break;
      length += bytesRead;
    }
    if (length > current.size) throw new Error('File changed while reading; retry');
    data = buffer.subarray(0, length);
  } finally { await file.close(); }
  if (input.operation === 'download') return { name: path.basename(resolved), mime_type: mime(resolved), data: data.toString('base64') };
  if (input.operation === 'write') {
    if (hash(data) !== input.expected_revision) throw new Error('File changed since it was opened; reload before saving');
    const replacement = Buffer.from(input.content, 'utf8');
    if (replacement.length > MAX_FILE) throw new Error('File exceeds 4 MiB');
    const temporary = path.join(path.dirname(resolved), '.xpressclaw-' + crypto.randomUUID());
    try {
      await fs.writeFile(temporary, replacement, { flag: 'wx', mode: details.mode & 0o777 });
      await fs.rename(temporary, resolved);
    } finally { await fs.rm(temporary, { force: true }); }
    data = replacement;
  }
  const content = new TextDecoder('utf-8', { fatal: true }).decode(data);
  if (content.includes('\0')) throw new Error('Binary file; use Download');
  return { path: requested, content, revision: hash(data), size: data.length };
}
let input = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', data => {
  input += data;
  if (input.length > 8 * 1024 * 1024) process.exit(1);
  if (input.endsWith('\n')) {
    process.stdin.pause();
    main(JSON.parse(input)).then(result => { process.stdout.write(JSON.stringify(result)); }, error => { process.stdout.write(JSON.stringify({ error: error.message })); }).finally(() => process.stdin.destroy());
  }
});
