#!/usr/bin/env node

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const revisionPattern = /^[0-9a-f]{40}$/;

export function planRelease({ workspaceVersion, tags, tag, stableRelease }) {
	if (!versionPattern.test(workspaceVersion))
		throw new Error('Invalid workspace version');
	if (tag !== undefined) {
		const version = tag.startsWith('v') ? tag.slice(1) : '';
		if (
			!versionPattern.test(version) ||
			version !== workspaceVersion ||
			stableRelease?.version !== version
		) {
			throw new Error(
				'Stable tag must match the workspace version and .github/stable-release.json'
			);
		}
		const sourceVersion = stableRelease.source_tag?.startsWith('v')
			? stableRelease.source_tag.slice(1)
			: '';
		if (
			!versionPattern.test(sourceVersion) ||
			!revisionPattern.test(stableRelease.source_sha ?? '') ||
			!revisionPattern.test(stableRelease.runner_tag ?? '')
		) {
			throw new Error(
				'Stable releases require a pinned source prerelease and runner revision'
			);
		}
		const sourceParts = sourceVersion.split('.').map(Number);
		const targetParts = version.split('.').map(Number);
		const difference = targetParts
			.map((part, index) => part - sourceParts[index])
			.find((value) => value !== 0);
		if (difference === undefined || difference < 0)
			throw new Error(
				'Stable version must be newer than its source prerelease'
			);
		return {
			version,
			build: String(sourceParts[2]),
			tag,
			prerelease: 'false',
			runner_tag: stableRelease.runner_tag,
			source_tag: stableRelease.source_tag,
			source_sha: stableRelease.source_sha
		};
	}
	const builds = tags.flatMap((existingTag) => {
		const match =
			existingTag.match(/^beta-(\d+)$/) ??
			existingTag.match(/^v.*-build\.(\d+)$/) ??
			existingTag.match(/^v\d+\.\d+\.(\d+)$/);
		return match ? [Number(match[1])] : [];
	});
	const build = Math.max(0, ...builds) + 1;
	if (!Number.isSafeInteger(build) || build > 65535)
		throw new Error('Build number exceeds the Windows package patch limit');
	const version = `${workspaceVersion.split('.').slice(0, 2).join('.')}.${build}`;
	return {
		version,
		build: String(build),
		tag: `v${version}`,
		prerelease: 'true',
		source_tag: ''
	};
}

export function computeReleasePlan(root, env = process.env) {
	const git = (...args) =>
		execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
	const workspaceVersion = execFileSync(
		process.execPath,
		[join(root, 'scripts/release-metadata.mjs'), '--version'],
		{ encoding: 'utf8' }
	).trim();
	const tag = env.GITHUB_REF_TYPE === 'tag' ? env.GITHUB_REF_NAME : undefined;
	const stableRelease =
		tag === undefined
			? undefined
			: JSON.parse(
					readFileSync(join(root, '.github/stable-release.json'), 'utf8')
				);
	const plan = planRelease({
		workspaceVersion,
		tags: git('tag', '-l').split('\n'),
		tag,
		stableRelease
	});
	if (plan.source_sha) {
		const source = git(
			'rev-parse',
			'--verify',
			`refs/tags/${plan.source_tag}^{commit}`
		);
		if (source !== plan.source_sha)
			throw new Error('Source prerelease tag has moved from its pinned commit');
		git('merge-base', '--is-ancestor', source, 'HEAD');
	} else {
		plan.runner_tag = execFileSync(
			'bash',
			[join(root, 'scripts/runner-revision.sh')],
			{ cwd: root, encoding: 'utf8' }
		).trim();
	}
	if (!revisionPattern.test(plan.runner_tag))
		throw new Error('Invalid runner revision');
	plan.sha = git('rev-parse', '--short', 'HEAD');
	return plan;
}

if (
	process.argv[1] &&
	resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
	const root = join(dirname(fileURLToPath(import.meta.url)), '..');
	for (const [key, value] of Object.entries(computeReleasePlan(root)))
		console.log(`${key}=${value}`);
}
