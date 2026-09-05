#!/usr/bin/env node

import { readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';

const manifestPath = fileURLToPath(
  new URL('../harnesses/runner-versions.json', import.meta.url),
);

export const expectedRunnerIds = [
  'codex',
  'claude',
  'deepseek-harness',
  'github-copilot',
  'junie',
  'kimi',
  'opencode',
  'pi',
  'qwen',
  'cline',
  'cursor',
  'glm',
  'grok',
  'kilo',
  'mistral-vibe',
];

function fail(message) {
  throw new Error(`runner versions: ${message}`);
}

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isExactVersion(value) {
  return (
    typeof value === 'string' &&
    /^\d+(?:\.\d+)+(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?(?:\+[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/.test(
      value,
    )
  );
}

function exactPackageVersion(spec) {
  const separator = spec.lastIndexOf('@');
  if (separator <= 0 || separator === spec.length - 1) return null;
  const version = spec.slice(separator + 1);
  return isExactVersion(version) ? version : null;
}

function packageName(spec) {
  return exactPackageVersion(spec) ? spec.slice(0, spec.lastIndexOf('@')) : null;
}

export function validateRunnerVersions(manifest) {
  if (!isRecord(manifest) || manifest.schema_version !== 1) {
    fail('schema_version must be 1');
  }
  if (typeof manifest.registry_url !== 'string' || !manifest.registry_url.startsWith('https://')) {
    fail('registry_url must be an HTTPS URL');
  }
  if (!isRecord(manifest.runners)) fail('runners must be an object');

  const ids = Object.keys(manifest.runners);
  const missing = expectedRunnerIds.filter((id) => !ids.includes(id));
  const extra = ids.filter((id) => !expectedRunnerIds.includes(id));
  if (missing.length || extra.length) {
    fail(`runner set differs (missing: ${missing.join(', ') || 'none'}; extra: ${extra.join(', ') || 'none'})`);
  }

  for (const [id, runner] of Object.entries(manifest.runners)) {
    if (!isRecord(runner)) fail(`${id} must be an object`);
    if (typeof runner.auto_update !== 'boolean') fail(`${id}.auto_update must be boolean`);
    if (
      !runner.auto_update &&
      (typeof runner.pin_reason !== 'string' || !runner.pin_reason.trim())
    ) {
      fail(`${id}.pin_reason is required while automatic updates are disabled`);
    }
    if (runner.auto_update && 'pin_reason' in runner) {
      fail(`${id}.pin_reason must be removed when automatic updates are enabled`);
    }
    if (!isExactVersion(runner.version)) {
      fail(`${id}.version must be an exact version`);
    }
    if (
      !Array.isArray(runner.acp_command) ||
      runner.acp_command.length === 0 ||
      runner.acp_command.some(
        (part) => typeof part !== 'string' || !part || part.includes('\n') || part.includes('\r'),
      )
    ) {
      fail(`${id}.acp_command must be a non-empty array of single-line strings`);
    }
    if (!['codex', 'claude', 'opencode', 'npm', 'binary'].includes(runner.dockerfile)) {
      fail(`${id}.dockerfile is unsupported`);
    }
    if (!Array.isArray(runner.sources) || runner.sources.length === 0) {
      fail(`${id}.sources must be a non-empty array`);
    }
    if (!isRecord(runner.build_args)) fail(`${id}.build_args must be an object`);
    if ('RUNNER_VERSION' in runner.build_args) {
      fail(`${id}.build_args must not define the generated RUNNER_VERSION argument`);
    }
    for (const name of ['ACP_SMOKE_COMMAND', 'ACP_SMOKE_COMMAND_B64']) {
      if (name in runner.build_args) {
        fail(`${id}.build_args must not define the generated ${name} argument`);
      }
    }

    for (const [name, value] of Object.entries(runner.build_args)) {
      if (!/^[A-Z][A-Z0-9_]*$/.test(name)) fail(`${id} has invalid build argument ${name}`);
      if (typeof value !== 'string' || value.includes('\n') || value.includes('\r')) {
        fail(`${id}.${name} must be a single-line string`);
      }
    }

    const primarySources = runner.sources.filter((source) => source?.primary !== false);
    if (primarySources.length !== 1) fail(`${id} must have exactly one primary version source`);

    for (const source of runner.sources) {
      if (!isRecord(source) || !['acp-registry', 'npm'].includes(source.kind)) {
        fail(`${id} has an unsupported source`);
      }
      if ('primary' in source && typeof source.primary !== 'boolean') {
        fail(`${id} source primary flag must be boolean`);
      }
      if (!['version', 'package', 'binary'].includes(source.target)) {
        fail(`${id} has an unsupported source target`);
      }
      if (source.kind === 'acp-registry' && typeof source.id !== 'string') {
        fail(`${id} ACP Registry source requires id`);
      }
      if (source.kind === 'npm' && typeof source.package !== 'string') {
        fail(`${id} npm source requires package`);
      }
      if (source.target !== 'binary') {
        if (typeof source.arg !== 'string' || !(source.arg in runner.build_args)) {
          fail(`${id} source target requires an existing build argument`);
        }
        if (source.target === 'package' && !exactPackageVersion(runner.build_args[source.arg])) {
          fail(`${id}.${source.arg} must contain an exact package version`);
        }
      }
    }

    const primary = primarySources[0];
    if (primary.target === 'version' && runner.build_args[primary.arg] !== runner.version) {
      fail(`${id}.${primary.arg} must match its runner version`);
    }
    if (
      primary.target === 'package' &&
      exactPackageVersion(runner.build_args[primary.arg]) !== runner.version
    ) {
      fail(`${id}.${primary.arg} package version must match its runner version`);
    }

    if (runner.dockerfile === 'binary') {
      for (const name of [
        'AGENT_PATH',
        'AGENT_ARCHIVE_AMD64',
        'AGENT_ARCHIVE_ARM64',
        'AGENT_SHA256_AMD64',
        'AGENT_SHA256_ARM64',
      ]) {
        if (!(name in runner.build_args)) fail(`${id} binary runner is missing ${name}`);
      }
      if (
        runner.build_args.AGENT_PATH.startsWith('/') ||
        runner.build_args.AGENT_PATH.split('/').includes('..')
      ) {
        fail(`${id}.AGENT_PATH must be a relative path inside the archive`);
      }
      for (const name of ['AGENT_ARCHIVE_AMD64', 'AGENT_ARCHIVE_ARM64']) {
        if (!runner.build_args[name].startsWith('https://')) {
          fail(`${id}.${name} must be an HTTPS URL`);
        }
      }
      for (const name of ['AGENT_SHA256_AMD64', 'AGENT_SHA256_ARM64']) {
        if (runner.build_args[name] && !/^[0-9a-f]{64}$/.test(runner.build_args[name])) {
          fail(`${id}.${name} must be empty or a lowercase SHA-256`);
        }
      }
    }
  }
  return manifest;
}

function registryAgent(registry, id) {
  const agents = registry?.agents;
  if (!Array.isArray(agents)) fail('ACP Registry response has no agents array');
  const agent = agents.find((candidate) => candidate.id === id);
  if (!agent) fail(`ACP Registry has no ${id} entry`);
  if (typeof agent.version !== 'string' || !agent.version) {
    fail(`ACP Registry entry ${id} has no version`);
  }
  return agent;
}

function binaryTarget(agent, platform) {
  const target = agent.distribution?.binary?.[platform];
  if (!isRecord(target) || typeof target.archive !== 'string' || typeof target.cmd !== 'string') {
    fail(`ACP Registry entry ${agent.id} has no complete ${platform} binary`);
  }
  return target;
}

function packageFor(agent) {
  const spec = agent.distribution?.npx?.package;
  if (typeof spec !== 'string' || !exactPackageVersion(spec)) {
    fail(`ACP Registry entry ${agent.id} has no exact npx package`);
  }
  return spec;
}

export async function updateRunnerVersions(
  input,
  {
    registry,
    npmVersion,
  },
) {
  const manifest = structuredClone(input);
  validateRunnerVersions(manifest);

  for (const [id, runner] of Object.entries(manifest.runners)) {
    if (!runner.auto_update) continue;
    let primaryVersion = null;

    for (const source of runner.sources) {
      let version;
      let agent;
      if (source.kind === 'acp-registry') {
        agent = registryAgent(registry, source.id);
        version = agent.version;
      } else {
        version = await npmVersion(source.package);
        if (typeof version !== 'string' || !version) {
          fail(`npm returned no version for ${source.package}`);
        }
      }

      if (source.primary !== false && primaryVersion === null) primaryVersion = version;

      if (source.target === 'version') {
        runner.build_args[source.arg] = version;
      } else if (source.target === 'package') {
        const nextPackage =
          source.kind === 'acp-registry' ? packageFor(agent) : `${source.package}@${version}`;
        if (packageName(nextPackage) !== packageName(runner.build_args[source.arg])) {
          fail(`${id} source changed package identity to ${packageName(nextPackage)}`);
        }
        runner.build_args[source.arg] = nextPackage;
      } else {
        if (source.kind !== 'acp-registry') fail(`${id} binary source must use the ACP Registry`);
        const amd64 = binaryTarget(agent, 'linux-x86_64');
        const arm64 = binaryTarget(agent, 'linux-aarch64');
        runner.build_args.AGENT_ARCHIVE_AMD64 = amd64.archive;
        runner.build_args.AGENT_ARCHIVE_ARM64 = arm64.archive;
        runner.build_args.AGENT_SHA256_AMD64 = amd64.sha256 ?? '';
        runner.build_args.AGENT_SHA256_ARM64 = arm64.sha256 ?? '';

        const expectedPath = runner.build_args.AGENT_PATH;
        const amd64Path = amd64.cmd.replace(/^\.\//, '');
        const arm64Path = arm64.cmd.replace(/^\.\//, '');
        if (amd64Path !== expectedPath || arm64Path !== expectedPath) {
          fail(
            `${id} registry command changed (expected ${expectedPath}; got ${amd64.cmd} and ${arm64.cmd})`,
          );
        }
      }
    }

    if (primaryVersion === null) fail(`${id} has no primary version source`);
    runner.version = primaryVersion;
  }

  return validateRunnerVersions(manifest);
}

export function runnerMatrix(manifest) {
  validateRunnerVersions(manifest);
  return Object.entries(manifest.runners).map(([id, runner]) => ({
    id,
    version: runner.version,
    dockerfile: runner.dockerfile,
    build_args: Object.entries({
      ...runner.build_args,
      ACP_SMOKE_COMMAND_B64: Buffer.from(JSON.stringify(runner.acp_command)).toString('base64'),
      RUNNER_VERSION: runner.version,
    })
      .map(([name, value]) => `${name}=${value}`)
      .join('\n'),
  }));
}

async function loadManifest() {
  return JSON.parse(await readFile(manifestPath, 'utf8'));
}

async function fetchJson(url, label) {
  const response = await fetch(url, {
    headers: { 'user-agent': 'xpressclaw-runner-version-updater' },
  });
  if (!response.ok) fail(`${label} returned HTTP ${response.status}`);
  return response.json();
}

async function fetchNpmVersion(packageName) {
  const encoded = packageName.replace('/', '%2f');
  const metadata = await fetchJson(`https://registry.npmjs.org/${encoded}/latest`, packageName);
  return metadata.version;
}

async function main() {
  const command = process.argv[2] ?? 'validate';
  const manifest = await loadManifest();

  if (command === 'validate') {
    validateRunnerVersions(manifest);
    console.log(`Validated ${expectedRunnerIds.length} pinned runner versions.`);
    return;
  }
  if (command === 'list') {
    validateRunnerVersions(manifest);
    console.log(expectedRunnerIds.join('\n'));
    return;
  }
  if (command === 'dockerfile') {
    validateRunnerVersions(manifest);
    const id = process.argv[3];
    const runner = manifest.runners[id];
    if (!runner) fail(`unknown runner ${id ?? ''}`);
    console.log(runner.dockerfile);
    return;
  }
  if (command === 'build-args') {
    const id = process.argv[3];
    const runner = runnerMatrix(manifest).find((candidate) => candidate.id === id);
    if (!runner) fail(`unknown runner ${id ?? ''}`);
    console.log(runner.build_args);
    return;
  }
  if (command === 'matrix') {
    console.log(JSON.stringify(runnerMatrix(manifest)));
    return;
  }
  if (command === 'update') {
    const registry = await fetchJson(manifest.registry_url, 'ACP Registry');
    const updated = await updateRunnerVersions(manifest, {
      registry,
      npmVersion: fetchNpmVersion,
    });
    await writeFile(manifestPath, `${JSON.stringify(updated, null, 2)}\n`);
    console.log('Resolved current exact runner versions.');
    return;
  }
  fail(`unknown command ${command}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
