import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import {
	cpSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { computeReleasePlan, planRelease } from './release-plan.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const metadataFiles = [
	'Cargo.toml',
	'Cargo.lock',
	'crates/xpressclaw-tauri/tauri.conf.json',
	'frontend/package.json',
	'frontend/package-lock.json',
	...['apps', 'memory', 'skills', 'tasks', 'workflows', 'xpressclaw'].map(
		(name) => `harnesses/base/mcp_${name}.py`
	),
	'harnesses/native/common/mcp-github.mjs',
	'harnesses/native/common/mcp-xpressclaw.mjs'
];
const stableRelease = {
	version: '0.3.0',
	source_tag: 'v0.2.123',
	source_sha: 'f7a8d7c4a5e6e9c056f102f24c370bf719149c50',
	runner_tag: '47143b73d463407cf525cf753972ff311f8a8f17'
};

function fixture(t) {
	const dir = mkdtempSync(join(tmpdir(), 'xpressclaw-release-test-'));
	t.after(() => rmSync(dir, { recursive: true, force: true }));
	for (const file of [
		...metadataFiles,
		'scripts/release-metadata.mjs',
		'scripts/runner-revision.sh'
	]) {
		mkdirSync(dirname(join(dir, file)), { recursive: true });
		cpSync(join(root, file), join(dir, file));
	}
	return dir;
}

function stamp(dir, ...args) {
	return execFileSync(
		process.execPath,
		[join(dir, 'scripts/release-metadata.mjs'), ...args],
		{ encoding: 'utf8' }
	);
}

function workflowScript(name) {
	const workflow = readFileSync(
		join(root, '.github/workflows/release.yml'),
		'utf8'
	);
	const step = workflow
		.split(`      - name: ${name}\n`)[1]
		?.split('\n      - ')[0];
	const body = step?.split('        run: |\n')[1];
	assert.ok(body, `Missing workflow script: ${name}`);
	return body
		.split('\n')
		.map((line) => line.replace(/^          /, ''))
		.join('\n');
}

test('publication requires every platform asset and rejects a corrupt download', (t) => {
	const dir = fixture(t);
	const filenames = [
		'xpressclaw_0.3.0_aarch64.dmg',
		'xpressclaw_0.3.0_x64.dmg',
		'xpressclaw_0.3.0_x64_en-US.msi',
		'xpressclaw_0.3.0_x64-setup.exe',
		'xpressclaw_0.3.0_amd64.deb',
		'xpressclaw-0.3.0-1.x86_64.rpm',
		'xpressclaw-cli-aarch64-apple-darwin.tar.gz',
		'xpressclaw-cli-x86_64-apple-darwin.tar.gz',
		'xpressclaw-cli-x86_64-pc-windows-msvc.zip',
		'xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz'
	];
	mkdirSync(join(dir, 'artifacts'));
	for (const filename of filenames)
		writeFileSync(join(dir, 'artifacts', filename), 'fixture package');
	const env = { ...process.env, VERSION: '0.3.0' };
	execFileSync('bash', ['-e', '-c', workflowScript('Collect release assets')], {
		cwd: dir,
		env
	});
	const verify = () =>
		spawnSync(
			'bash',
			[
				'-e',
				'-c',
				workflowScript('Verify all platform downloads and checksums')
			],
			{ cwd: dir, env, encoding: 'utf8' }
		);
	assert.equal(verify().status, 0);
	writeFileSync(join(dir, 'release-assets', filenames[0]), 'corrupt');
	assert.notEqual(verify().status, 0);
	rmSync(join(dir, 'release-assets', filenames[0]));
	const missing = verify();
	assert.notEqual(missing.status, 0);
	assert.match(missing.stderr, /Missing release asset/);
});

test('stable and prerelease notes keep literal shell examples and the correct version', (t) => {
	const dir = fixture(t);
	mkdirSync(join(dir, 'docs/releases'), { recursive: true });
	cpSync(
		join(root, 'docs/releases/0.3.0.md'),
		join(dir, 'docs/releases/0.3.0.md')
	);
	for (const prerelease of ['false', 'true']) {
		execFileSync(
			'bash',
			['-e', '-c', workflowScript('Prepare release notes')],
			{
				cwd: dir,
				env: {
					...process.env,
					VERSION: '0.3.0',
					TAG: 'v0.3.0',
					RUNNER_TAG: stableRelease.runner_tag,
					GITHUB_SHA: stableRelease.source_sha,
					PRERELEASE: prerelease
				}
			}
		);
		const notes = readFileSync(join(dir, 'release-notes.md'), 'utf8');
		assert.match(
			notes,
			/```bash\ncurl -fsSL[^\n]+XPRESSCLAW_VERSION=v0\.3\.0 sh\n```/
		);
		assert.ok(
			notes.includes(
				`Runner images are pinned to \`${stableRelease.runner_tag}\`.`
			)
		);
		assert.equal(
			notes.includes('The first stable XpressClaw release'),
			prerelease === 'false'
		);
	}
});

test('stable stamping synchronizes app, lockfiles, and MCP versions while retaining the source build number', (t) => {
	const dir = fixture(t);
	stamp(dir, '--set-version', '0.2.0');
	stamp(dir, '--build', '123');
	assert.equal(stamp(dir, '--version').trim(), '0.2.123');
	const dependencyVersions = readFileSync(
		join(dir, 'Cargo.lock'),
		'utf8'
	).replace(/(name = "xpressclaw-[^"]+"\nversion = ")[^"]+/g, '$1APP');
	stamp(dir, '--set-version', '0.3.0');
	stamp(dir, '--build', '123', '--version', '0.3.0');
	assert.match(stamp(dir, '--check'), /synchronized at 0\.3\.0/);
	assert.equal(stamp(dir, '--version').trim(), '0.3.0');
	assert.equal(
		JSON.parse(
			readFileSync(join(dir, 'crates/xpressclaw-tauri/tauri.conf.json'))
		).bundle.macOS.bundleVersion,
		'123'
	);
	assert.equal(
		readFileSync(join(dir, 'Cargo.lock'), 'utf8').replace(
			/(name = "xpressclaw-[^"]+"\nversion = ")[^"]+/g,
			'$1APP'
		),
		dependencyVersions
	);
	stamp(dir, '--build', '124');
	assert.match(stamp(dir, '--check'), /synchronized at 0\.3\.124/);
});

test('invalid explicit versions and incomplete overrides do not change metadata', (t) => {
	const dir = fixture(t);
	const before = metadataFiles.map((file) =>
		readFileSync(join(dir, file), 'utf8')
	);
	for (const args of [
		['--set-version', '0.3.0-rc.1'],
		['--set-version', '00.3.0'],
		['--build', '123', '--version'],
		['--build', '123', '--version', '0.3.0;exit'],
		['--build', '123', '--unknown', '0.3.0'],
		['--build', '-1', '--version', '0.3.0']
	]) {
		const result = spawnSync(process.execPath, [
			join(dir, 'scripts/release-metadata.mjs'),
			...args
		]);
		assert.notEqual(result.status, 0, args.join(' '));
		assert.deepEqual(
			metadataFiles.map((file) => readFileSync(join(dir, file), 'utf8')),
			before
		);
	}
});

test('main builds remain prereleases and advance past all historic build tag formats', () => {
	assert.deepEqual(
		planRelease({
			workspaceVersion: '0.3.0',
			tags: ['beta-30', 'v0.2.0-build.45', 'v0.2.123', 'v0.3.0', 'irrelevant']
		}),
		{
			version: '0.3.124',
			build: '124',
			tag: 'v0.3.124',
			prerelease: 'true',
			source_tag: ''
		}
	);
	assert.throws(
		() => planRelease({ workspaceVersion: '0.3.0', tags: ['v0.3.65535'] }),
		/Windows package/
	);
});

test('stable promotion uses the exact version and tested runner/build metadata', () => {
	assert.deepEqual(
		planRelease({
			workspaceVersion: '0.3.0',
			tags: ['v0.2.123', 'v0.3.124'],
			tag: 'v0.3.0',
			stableRelease
		}),
		{
			version: '0.3.0',
			build: '123',
			tag: 'v0.3.0',
			prerelease: 'false',
			runner_tag: stableRelease.runner_tag,
			source_tag: stableRelease.source_tag,
			source_sha: stableRelease.source_sha
		}
	);
});

test('stable promotion rejects unconfigured tags, mismatched versions, and incomplete provenance', () => {
	const base = {
		workspaceVersion: '0.3.0',
		tags: [],
		tag: 'v0.3.0',
		stableRelease
	};
	for (const overrides of [
		{ stableRelease: undefined },
		{ workspaceVersion: '0.2.0' },
		{ tag: 'v0.3.1' },
		{ tag: 'v0.3.0-rc.1' },
		{ tag: '../0.3.0' },
		{ stableRelease: { ...stableRelease, runner_tag: 'latest' } },
		{ stableRelease: { ...stableRelease, source_sha: 'main' } },
		{ stableRelease: { ...stableRelease, source_tag: 'v0.3.0' } },
		{ stableRelease: { ...stableRelease, source_tag: 'v0.3.124' } }
	])
		assert.throws(() => planRelease({ ...base, ...overrides }));
});

test('release planning verifies the source tag before trusting the pinned image revision', (t) => {
	const dir = fixture(t);
	const git = (...args) =>
		execFileSync('git', args, { cwd: dir, encoding: 'utf8' }).trim();
	git('init', '-q');
	git('config', 'user.name', 'Release test');
	git('config', 'user.email', 'release-test@example.invalid');
	git('add', '.');
	git('commit', '-qm', 'Prerelease source');
	git('tag', 'v0.2.123');
	const sourceSha = git('rev-parse', 'HEAD');
	mkdirSync(join(dir, '.github'));
	writeFileSync(
		join(dir, '.github/stable-release.json'),
		JSON.stringify({ ...stableRelease, source_sha: sourceSha })
	);
	git('add', '.');
	git('commit', '-qm', 'Release metadata');
	const prereleasePlan = computeReleasePlan(dir, { GITHUB_REF_TYPE: 'branch' });
	assert.equal(prereleasePlan.runner_tag, sourceSha);
	assert.equal(prereleasePlan.prerelease, 'true');
	const env = { GITHUB_REF_TYPE: 'tag', GITHUB_REF_NAME: 'v0.3.0' };
	const plan = computeReleasePlan(dir, env);
	assert.equal(plan.source_sha, sourceSha);
	assert.equal(plan.runner_tag, stableRelease.runner_tag);
	assert.equal(plan.version, '0.3.0');
	git('tag', '-f', 'v0.2.123');
	assert.throws(() => computeReleasePlan(dir, env), /tag has moved/);
});
