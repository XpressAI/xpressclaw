// Executed inside the retained Agent container, alongside the Files helper.
const fs = require('node:fs');
const path = require('node:path');
const { createHash } = require('node:crypto');
const { spawnSync } = require('node:child_process');

const maxFiles = 10000;
const maxOutput = 4 * 1024 * 1024;
const maxHashBytes = 128 * 1024 * 1024;

function git(dir, args, encoding = 'utf8') {
  const result = spawnSync('git', ['--literal-pathspecs', '-c', 'core.fsmonitor=false', '-C', dir, ...args], {
    env: { ...process.env, GIT_OPTIONAL_LOCKS: '0', LC_ALL: 'C' },
    encoding, timeout: 8000, maxBuffer: maxOutput,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(result.stderr.toString().trim() || 'Git could not inspect this directory');
  return result.stdout;
}

// Only advertise a saved version when the commit actually contains a blob.
// This also keeps the file readable if the linked worktree is later removed.
function blobVersions(dir, common, commit, names) {
  const objects = new Map();
  if (!commit) return objects;
  for (let i = 0; i < names.length; i += 128) {
    const entries = git(dir, ['ls-tree', '-z', commit, '--', ...names.slice(i, i + 128)]);
    for (const entry of entries.split('\0')) {
      const tab = entry.indexOf('\t');
      if (tab < 0) continue;
      const [, type, oid] = entry.slice(0, tab).split(' ');
      if (type === 'blob') objects.set(entry.slice(tab + 1), { common, object: oid });
    }
  }
  return objects;
}

function repository(dir) {
  let prefix;
  try { prefix = git(dir, ['rev-parse', '--show-prefix']).replace(/\n$/, ''); }
  catch (error) {
    if (error.code === 'ENOENT' || /not a git repository/i.test(error.message)) return null;
    throw error;
  }
  const common = git(dir, ['rev-parse', '--path-format=absolute', '--git-common-dir']).replace(/\n$/, '');
  return { common, prefix };
}

function pathsChangedSince(dir, base) {
  const fields = git(dir, ['diff', '--relative', '--no-renames', '--no-ext-diff', '--no-textconv', '--ignore-submodules=all',
    '--name-status', '-z', base, '--', '.']).split('\0');
  const changes = new Map();
  for (let i = 0; i + 1 < fields.length; i += 2) changes.set(fields[i + 1], fields[i]);
  return changes;
}

function workingChanges(dir, prefix, omitted, previouslyOmitted) {
  const result = new Map();
  const fields = git(dir, ['status', '--porcelain=v1', '-z', '--no-renames', '--ignore-submodules=all', '--untracked-files=normal', '--', '.']);
  for (const record of fields.split('\0').filter(Boolean)) {
    const full = record.slice(3);
    if (!full.startsWith(prefix)) throw new Error('Git returned a path outside the registered directory');
    const name = full.slice(prefix.length);
    if (!full.endsWith('/')) {
      result.set(name, record.slice(0, 2));
      continue;
    }
    if (previouslyOmitted.has(name)) {
      omitted.push(name);
      continue;
    }
    // A large cache should not prevent small new source directories from being listed.
    try {
      // When the entire registered subdirectory is untracked, Git reports
      // the directory itself. Its path relative to our cwd is ".".
      const children = git(dir, ['ls-files', '--others', '--exclude-standard', '-z', '--', name || '.']).split('\0').filter(Boolean);
      if (children.length + result.size > maxFiles) {
        if (!name) throw new Error('Too many untracked files. Choose a smaller task directory.');
        omitted.push(name);
      } else {
        for (const child of children) if (!child.endsWith('/')) result.set(child, '??');
      }
    } catch (error) {
      if (error.code !== 'ENOBUFS') throw error;
      if (!name) throw new Error('Too many untracked files. Choose a smaller task directory.');
      omitted.push(name);
    }
  }
  return result;
}

function plainFiles(dir) {
  const files = new Map();
  let entries = 0;
  function visit(parent) {
    const handle = fs.opendirSync(path.join(dir, parent));
    try {
      for (let child; (child = handle.readSync()) !== null;) {
        if (++entries > maxFiles) throw new Error('Too many entries. Choose a smaller task directory.');
        const name = path.join(parent, child.name);
        if (child.name === '.git') continue;
        if (child.isDirectory()) visit(name);
        else if (child.isFile() || child.isSymbolicLink()) files.set(name, '??');
      }
    } finally { handle.closeSync(); }
  }
  visit('');
  return files;
}

function stamp(dir, name, budget) {
  const filename = path.join(dir, name);
  // A previously observed parent directory may have been removed, replaced
  // by a file, or replaced by a symlink between samples.
  const parent = path.dirname(filename);
  try {
    if (fs.realpathSync(parent) !== parent) return null;
  } catch (error) {
    if (error.code === 'ENOENT' || error.code === 'ENOTDIR') return null;
    throw error;
  }
  const info = fs.lstatSync(filename, { bigint: true, throwIfNoEntry: false });
  if (!info) return null;
  if (info.isSymbolicLink()) return 'link:' + fs.readlinkSync(filename);
  if (!info.isFile() || info.size > BigInt(maxOutput)) return [info.mode, info.size, info.mtimeNs].join(':');
  budget.bytes += Number(info.size);
  if (budget.bytes > maxHashBytes) throw new Error('Too much changed content to scan. Choose a smaller task directory.');
  const fd = fs.openSync(filename, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
  try {
    if (!fs.fstatSync(fd).isFile()) throw new Error('File changed type while being inspected');
    const hash = createHash('sha256');
    const buf = Buffer.alloc(65536);
    // A concurrent writer cannot make the read grow beyond the inspected size.
    let remaining = Number(info.size);
    while (remaining > 0) {
      const bytes = fs.readSync(fd, buf, 0, Math.min(buf.length, remaining), null);
      if (!bytes) break;
      hash.update(buf.subarray(0, bytes));
      remaining -= bytes;
    }
    return info.mode + ':' + hash.digest('hex');
  } finally { fs.closeSync(fd); }
}

function inspect(input) {
  if (input.operation === 'directory_version') {
    const data = git(input.version.common, ['--git-dir=.', 'cat-file', 'blob', input.version.object], 'buffer');
    if (data.includes(0)) throw new Error('This version is not a text file');
    return { content: new TextDecoder('utf-8', { fatal: true }).decode(data) };
  }
  if (typeof input.path !== 'string' || !path.isAbsolute(input.path)) throw new Error('An absolute container directory is required');
  const dir = fs.realpathSync(input.path);
  if (dir === '/' || /^\/(proc|dev|sys)(\/|$)/.test(dir) || !fs.statSync(dir).isDirectory()) {
    throw new Error('Choose a local directory below the container root and outside device filesystems');
  }
  // A folder registered before `git init` keeps its filesystem baseline for
  // the current turn. A subsequent turn can start using its new repository.
  const repo = input.baseline && !input.baseline.repo ? null : repository(dir);
  const omitted = [];
  const working = repo ? workingChanges(dir, repo.prefix, omitted, new Set(input.baseline?.omitted)) : plainFiles(dir);
  const budget = { bytes: 0 };
  if (working.size > maxFiles) throw new Error('Too many changes. Choose a smaller task directory.');
  if (input.operation === 'directory_baseline') {
    let head = null;
    if (repo) {
      // An unborn repository starts at Git's empty tree, including SHA-256 repositories.
      const ref = git(dir, ['rev-parse', '--revs-only', 'HEAD']).trim();
      head = ref || git(dir, ['hash-object', '-t', 'tree', '--stdin']).trim();
    }
    const initial = Array.from(working.keys(), name => [name, stamp(dir, name, budget)]);
    return { path: dir, baseline: { repo, head, initial, omitted }, omitted };
  }
  const baseline = input.baseline;
  if (dir !== input.path || repo?.common !== baseline.repo?.common || repo?.prefix !== baseline.repo?.prefix) {
    throw new Error('The registered directory or its repository has changed');
  }
  const initial = new Map(baseline.initial);
  // Comparing to the original commit keeps tracked changes visible after commit.
  const changes = repo ? pathsChangedSince(dir, baseline.head) : new Map();
  for (const [name, status] of working) {
    if (!changes.has(name)) changes.set(name, status === '??' ? 'A' : status.trim().slice(-1));
  }
  const skipped = [...new Set([...baseline.omitted, ...omitted])];
  const committedWithoutEdits = [];
  for (const [name, before] of initial) {
    const after = stamp(dir, name, budget);
    if (before === after) {
      // An earlier turn's edit can become a clean, committed file in this
      // turn. Supply its blob without claiming a new edit to that path.
      if (repo && after !== null && !working.has(name) && changes.has(name)) committedWithoutEdits.push(name);
      changes.delete(name);
    } else {
      changes.set(name, after === null ? 'D' : before === null ? 'A' : 'M');
    }
  }
  if (changes.size > maxFiles) throw new Error('Too many changes. Choose a smaller task directory.');
  const included = name => !skipped.some(prefix => name.startsWith(prefix));
  const files = [...changes].filter(([name]) => included(name))
    .map(([path, status]) => ({ path, status }));
  let committedVersions = {};
  const tip = repo && git(dir, ['rev-parse', '--revs-only', 'HEAD']).trim();
  if (repo) {
    const removed = blobVersions(dir, repo.common, baseline.head, files.filter(f => f.status === 'D').map(f => f.path));
    const committed = blobVersions(dir, repo.common, tip, files.filter(f => f.status !== 'D' && !working.has(f.path)).map(f => f.path));
    for (const file of files) {
      file.version = removed.get(file.path) ?? committed.get(file.path);
    }
    committedVersions = Object.fromEntries(blobVersions(dir, repo.common, tip, committedWithoutEdits.filter(included)));
  }
  return { files, omitted: skipped, committed_versions: committedVersions };
}

// The control plane sends one line and keeps stdin open until the reply.
const input = require('node:readline').createInterface({ input: process.stdin });
input.once('line', line => {
  try { process.stdout.write(JSON.stringify(inspect(JSON.parse(line)))); }
  catch (error) { process.stdout.write(JSON.stringify({ error: error.message })); }
  finally { input.close(); process.stdin.destroy(); }
});
