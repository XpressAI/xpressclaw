import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const script = fileURLToPath(new URL('./task-directory.cjs', import.meta.url));
function request(input) {
  return JSON.parse(execFileSync(process.execPath, [script], { input: JSON.stringify(input) }));
}
function inspect(input) {
  const result = request(input);
  assert.equal(result.error, undefined);
  return result;
}
function setup(t) {
  const root = mkdtempSync(join(tmpdir(), 'task-directory-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return root;
}
function git(dir, ...args) {
  return execFileSync('git', ['-C', dir, '-c', 'user.name=test', '-c', 'user.email=test@example.org', ...args]);
}
const begin = path => inspect({ operation: 'directory_baseline', path }).baseline;
const changes = (path, baseline) => inspect({ operation: 'directory_changes', path, baseline });
const statuses = result => Object.fromEntries(result.files.map(f => [f.path, f.status]));

test('tracks only the registered worktree subdirectory across commit and preserves blobs after removal', t => {
  const root = setup(t), repo = join(root, 'main'), linked = join(root, 'other checkout');
  mkdirSync(repo);
  git(repo, 'init', '-q');
  mkdirSync(join(repo, 'src'));
  for (const name of ['gone', 'dirty', 'edited again']) writeFileSync(join(repo, 'src', name), 'initial');
  git(repo, 'add', '.');
  git(repo, 'commit', '-qm', 'initial');
  git(repo, 'worktree', 'add', '-qb', 'feature', linked);
  const dir = join(linked, 'src');
  writeFileSync(join(dir, 'dirty'), 'pre-existing');
  writeFileSync(join(dir, 'edited again'), 'pre-existing');
  writeFileSync(join(dir, 'scratch'), 'pre-existing');
  const base = begin(dir);
  assert.deepEqual(statuses(changes(dir, base)), {});
  writeFileSync(join(linked, 'elsewhere'), 'unrelated');
  writeFileSync(join(dir, 'edited again'), 'task edit');
  writeFileSync(join(dir, 'line\nbreak.ts'), 'new source');
  rmSync(join(dir, 'gone'));
  const expected = { gone: 'D', 'edited again': 'M', 'line\nbreak.ts': 'A' };
  const live = changes(dir, base);
  assert.deepEqual(statuses(live), expected);
  assert.equal(live.files.find(f => f.path === 'line\nbreak.ts').version, undefined);
  git(linked, 'add', '.');
  git(linked, 'commit', '-qm', 'work');
  const saved = changes(dir, base);
  assert.deepEqual(statuses(saved), expected);
  git(repo, 'worktree', 'remove', linked);
  for (const [name, content] of [['gone','initial'], ['line\nbreak.ts','new source']]) {
    const version = saved.files.find(f => f.path === name).version;
    assert.equal(inspect({ operation: 'directory_version', version }).content, content);
  }
});

test('a follow-up commit provides saved blobs for unchanged first-turn edits', t => {
  const root = setup(t), repo = join(root, 'repo'), dir = join(root, 'worktree');
  mkdirSync(repo);
  git(repo, 'init', '-q');
  writeFileSync(join(repo, 'edited.ts'), 'original');
  writeFileSync(join(repo, 'still dirty.ts'), 'original');
  git(repo, 'add', '.');
  git(repo, 'commit', '-qm', 'initial');
  git(repo, 'worktree', 'add', '-qb', 'feature', dir);

  const first = begin(dir);
  writeFileSync(join(dir, 'added.ts'), 'first-turn addition');
  writeFileSync(join(dir, 'edited.ts'), 'first-turn edit');
  writeFileSync(join(dir, 'still dirty.ts'), 'uncommitted');
  const initialChanges = changes(dir, first);
  assert.deepEqual(statuses(initialChanges), { 'added.ts': 'A', 'edited.ts': 'M', 'still dirty.ts': 'M' });
  assert.ok(initialChanges.files.every(file => !file.version));

  const followup = begin(dir);
  git(dir, 'add', 'added.ts', 'edited.ts');
  git(dir, 'commit', '-qm', 'commit the first turn');
  const saved = changes(dir, followup);
  assert.deepEqual(saved.files, []);
  assert.deepEqual(Object.keys(saved.committed_versions ?? {}).sort(), ['added.ts', 'edited.ts']);
  git(dir, 'restore', 'still dirty.ts');
  git(repo, 'worktree', 'remove', dir);
  for (const [name, content] of [['added.ts', 'first-turn addition'], ['edited.ts', 'first-turn edit']]) {
    assert.equal(inspect({ operation: 'directory_version', version: saved.committed_versions[name] }).content, content);
  }
});

test('tracks a plain directory, including creation of a repository during the task', t => {
  const dir = setup(t);
  writeFileSync(join(dir, '__proto__'), 'initial');
  writeFileSync(join(dir, 'gone'), 'original');
  const base = begin(dir);
  git(dir, 'init', '-q');
  writeFileSync(join(dir, '__proto__'), 'changed');
  rmSync(join(dir, 'gone'));
  mkdirSync(join(dir, 'folder'));
  writeFileSync(join(dir, 'folder/new'), 'created');
  assert.deepEqual(statuses(changes(dir, base)), { ['__proto__']: 'M', gone: 'D', 'folder/new': 'A' });
});

test('tracks individual files when the registered subdirectory is entirely untracked', t => {
  const root = setup(t);
  git(root, 'init', '-q');
  const dir = join(root, 'new project');
  mkdirSync(dir);
  writeFileSync(join(dir, 'existing'), 'keep');
  const base = begin(dir);
  writeFileSync(join(dir, 'source'), 'code');
  assert.deepEqual(statuses(changes(dir, base)), { source: 'A' });
  git(root, 'add', '.');
  git(root, 'commit', '-qm', 'project');
  assert.deepEqual(statuses(changes(dir, base)), { source: 'A' });
});

test('an unborn repository tracks its first commit and omits a large untracked cache', t => {
  const dir = setup(t);
  git(dir, 'init', '-q');
  const large = join(dir, 'cache');
  mkdirSync(large);
  for (let i = 0; i <= 10000; i++) writeFileSync(join(large, String(i)), '');
  const base = begin(dir);
  assert.deepEqual(base.omitted, ['cache/']);
  mkdirSync(join(dir, 'src'));
  writeFileSync(join(dir, 'src/main'), 'new');
  assert.deepEqual(statuses(changes(dir, base)), { 'src/main': 'A' });
  git(dir, 'add', 'src');
  git(dir, 'commit', '-qm', 'first');
  assert.deepEqual(statuses(changes(dir, base)), { 'src/main': 'A' });
});
