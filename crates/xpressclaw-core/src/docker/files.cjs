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
const MAX_UPLOAD = 20 * 1024 * 1024;
function downloadLimit(input) {
  const limit = input.max_bytes ?? MAX_DOWNLOAD;
  if (!Number.isSafeInteger(limit) || limit < 0 || limit > MAX_DOWNLOAD) throw new Error('Download limit must be between 0 and 100 MiB');
  return limit;
}
// Inspect metadata before reading bytes or starting tar. Do not follow links
// inside folders, matching tar's default behavior. Bound metadata work too.
async function downloadSize(filename, limit) {
  let size = 0;
  let entries = 0;
  async function visit(current) {
    if (++entries > 20000) throw new Error('Folder has too many entries to publish; download it from Files');
    const info = await fs.lstat(current);
    if (info.isDirectory()) {
      const directory = await fs.opendir(current);
      for await (const entry of directory) await visit(path.join(current, entry.name));
    } else {
      size += info.size;
      if (size > limit) throw new Error(`Files exceed the ${limit} byte download budget`);
    }
  }
  await visit(filename);
  return size;
}
function mime(filename) {
  return ({ '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.webp': 'image/webp', '.pdf': 'application/pdf', '.txt': 'text/plain', '.md': 'text/markdown', '.json': 'application/json', '.csv': 'text/csv', '.zip': 'application/zip', '.pptx': 'application/vnd.openxmlformats-officedocument.presentationml.presentation', '.docx': 'application/vnd.openxmlformats-officedocument.wordprocessingml.document', '.xlsx': 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet' })[path.extname(filename).toLowerCase()] ?? 'application/octet-stream';
}
async function main(input) {
  if (input.operation === 'stage') {
    const workspace = await fs.realpath(input.workspace);
    if (typeof input.data !== 'string' || input.data.length > Math.ceil(MAX_UPLOAD / 3) * 4) throw new Error('Upload exceeds 20 MiB');
    const data = Buffer.from(input.data, 'base64');
    if (data.length > MAX_UPLOAD) throw new Error('Upload exceeds 20 MiB');
    const characters = [...String(input.name || 'attachment')].slice(0, 180).map(character => /[\p{L}\p{N}._ -]/u.test(character) ? character : '_');
    while (Buffer.byteLength(characters.join('')) > 180) characters.pop();
    const name = characters.join('').replace(/^\.+$/, '') || 'attachment';
    // These are private files in the retained container, never host repository
    // files. Check real paths so even a workspace rooted in /var/tmp uses a
    // different staging root and a normal git add -A cannot include uploads.
    for (const candidate of ['/var/tmp', '/tmp']) {
      const root = await fs.realpath(candidate).catch(() => null);
      if (!root || workspace === '/' || root === workspace || root.startsWith(workspace + '/')) continue;
      const directory = await fs.mkdtemp(path.join(root, 'xpressclaw-uploads-')).catch(() => null);
      if (!directory) continue;
      try {
        const filename = path.join(directory, name);
        await fs.writeFile(filename, data, { flag: 'wx', mode: 0o600 });
        return { path: filename, size: data.length };
      } catch (error) {
        await fs.rm(directory, { recursive: true, force: true });
        throw error;
      }
    }
    throw new Error('No writable upload storage exists outside the container workspace');
  }
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
  if (input.operation === 'stat') {
    if (resolved === '/') throw new Error('Choose a folder below the container root');
    if (!details.isFile() && !details.isDirectory()) throw new Error('Not a regular file or folder');
    return { size: await downloadSize(resolved, downloadLimit(input)), kind: details.isDirectory() ? 'directory' : 'file' };
  }
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
    const limit = downloadLimit(input);
    if (input.max_bytes !== undefined) await downloadSize(resolved, limit);
    if (limit === 0) throw new Error('No download budget remains for this archive');
    const { stdout } = await promisify(execFile)('tar', ['-czf', '-', '-C', path.dirname(resolved), '--', path.basename(resolved)], { encoding: 'buffer', maxBuffer: limit, timeout: 60000 });
    return { name: path.basename(resolved) + '.tar.gz', mime_type: 'application/gzip', data: stdout.toString('base64') };
  }
  if (!details.isFile()) throw new Error('Not a regular file');
  const limit = input.operation === 'download' ? downloadLimit(input) : MAX_FILE;
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
  if (input.length > 32 * 1024 * 1024) process.exit(1);
  if (input.endsWith('\n')) {
    process.stdin.pause();
    main(JSON.parse(input)).then(result => { process.stdout.write(JSON.stringify(result)); }, error => { process.stdout.write(JSON.stringify({ error: error.message })); }).finally(() => process.stdin.destroy());
  }
});
