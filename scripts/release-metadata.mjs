#!/usr/bin/env node

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const read = (path) => readFileSync(join(root, path), 'utf8').replace(/\r\n?/g, '\n');
const write = (path, contents) => writeFileSync(join(root, path), contents);

const cargoToml = read('Cargo.toml');
const workspaceVersion = cargoToml.match(
	/^\[workspace\.package\]\s*\nversion\s*=\s*"([^"]+)"/m
)?.[1];

if (!workspaceVersion || !/^\d+\.\d+\.\d+$/.test(workspaceVersion)) {
	throw new Error('Cargo.toml must declare a numeric workspace version such as 0.2.0');
}

const mismatches = [];
const expectVersion = (label, actual) => {
	if (actual !== workspaceVersion) {
		mismatches.push(`${label}: expected ${workspaceVersion}, found ${actual ?? 'missing'}`);
	}
};

const tauriConfig = JSON.parse(read('crates/xpressclaw-tauri/tauri.conf.json'));
const frontendPackage = JSON.parse(read('frontend/package.json'));
const frontendLock = JSON.parse(read('frontend/package-lock.json'));

expectVersion('Tauri config', tauriConfig.version);
expectVersion('frontend package', frontendPackage.version);
expectVersion('frontend lockfile', frontendLock.version);
expectVersion('frontend lockfile root package', frontendLock.packages?.['']?.version);

const cargoLock = read('Cargo.lock');
const workspacePackages = new Set([
	'xpressclaw-cli',
	'xpressclaw-core',
	'xpressclaw-server',
	'xpressclaw-tauri'
]);
const lockVersions = new Map();
for (const block of cargoLock.split(/\n(?=\[\[package\]\]\n)/)) {
	const name = block.match(/^name = "([^"]+)"/m)?.[1];
	if (!name || !workspacePackages.has(name)) continue;
	lockVersions.set(name, block.match(/^version = "([^"]+)"/m)?.[1]);
}
for (const name of workspacePackages) {
	expectVersion(`Cargo.lock ${name}`, lockVersions.get(name));
}

const componentFiles = [
	'harnesses/base/mcp_apps.py',
	'harnesses/base/mcp_memory.py',
	'harnesses/base/mcp_skills.py',
	'harnesses/base/mcp_tasks.py',
	'harnesses/base/mcp_workflows.py',
	'harnesses/base/mcp_xpressclaw.py',
	'harnesses/native/common/mcp-github.mjs',
	'harnesses/native/common/mcp-xpressclaw.mjs'
];
const componentVersionPattern =
	/(serverInfo[\s\S]{0,200}?(?:["']version["']|version)\s*:\s*["'])([^"']+)(["'])/;
for (const path of componentFiles) {
	const version = read(path).match(componentVersionPattern)?.[2];
	if (!version) {
		mismatches.push(`${path}: version metadata is missing`);
		continue;
	}
	expectVersion(path, version);
}

if (mismatches.length) {
	console.error('Release version metadata is out of sync:');
	for (const mismatch of mismatches) console.error(`- ${mismatch}`);
	process.exit(1);
}

const buildNumber = (value, command) => {
	if (!value || !/^(0|[1-9]\d*)$/.test(value)) {
		throw new Error(`${command} requires a non-negative integer`);
	}
	return value;
};

const releaseVersion = (build) => {
	const [major, minor] = workspaceVersion.split('.');
	return `${major}.${minor}.${build}`;
};

const stampRelease = (build) => {
	const version = releaseVersion(build);
	const stampedCargoToml = cargoToml.replace(
		/^(\[workspace\.package\]\s*\nversion\s*=\s*")[^"]+(")/m,
		(_match, prefix, suffix) => `${prefix}${version}${suffix}`
	);
	const stampedCargoLock = cargoLock
		.split(/\n(?=\[\[package\]\]\n)/)
		.map((block) => {
			const name = block.match(/^name = "([^"]+)"/m)?.[1];
			if (!name || !workspacePackages.has(name)) return block;
			return block.replace(/^version = "[^"]+"/m, `version = "${version}"`);
		})
		.join('\n');

	tauriConfig.version = version;
	tauriConfig.bundle ??= {};
	tauriConfig.bundle.macOS ??= {};
	tauriConfig.bundle.macOS.bundleVersion = build;
	frontendPackage.version = version;
	frontendLock.version = version;
	frontendLock.packages[''].version = version;

	const stampedComponents = componentFiles.map((path) => {
		const contents = read(path);
		return [
			path,
			contents.replace(
				componentVersionPattern,
				(_match, prefix, _currentVersion, suffix) => `${prefix}${version}${suffix}`
			)
		];
	});

	write('Cargo.toml', stampedCargoToml);
	write('Cargo.lock', stampedCargoLock);
	write('crates/xpressclaw-tauri/tauri.conf.json', `${JSON.stringify(tauriConfig, null, 2)}\n`);
	write('frontend/package.json', `${JSON.stringify(frontendPackage, null, 2)}\n`);
	write('frontend/package-lock.json', `${JSON.stringify(frontendLock, null, 2)}\n`);
	for (const [path, contents] of stampedComponents) write(path, contents);

	return version;
};

const [command, value] = process.argv.slice(2);
if (command === '--check') {
	console.log(`Release version metadata is synchronized at ${workspaceVersion}.`);
} else if (command === '--version') {
	console.log(workspaceVersion);
} else if (command === '--release-version') {
	console.log(releaseVersion(buildNumber(value, '--release-version')));
} else if (command === '--build') {
	const build = buildNumber(value, '--build');
	console.log(`Stamped XpressClaw ${stampRelease(build)} (build ${build}).`);
} else {
	console.error(
		'Usage: release-metadata.mjs --check | --version | --release-version <build> | --build <number>'
	);
	process.exit(2);
}
