import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import {
  expectedRunnerIds,
  runnerMatrix,
  updateRunnerVersions,
  validateRunnerVersions,
} from './runner-versions.mjs';

const manifestUrl = new URL('../harnesses/runner-versions.json', import.meta.url);
const harnessWorkflowUrl = new URL('../.github/workflows/harnesses.yml', import.meta.url);
const releaseWorkflowUrl = new URL('../.github/workflows/release.yml', import.meta.url);

const expectedAcpCommands = {
  codex: ['codex-acp'],
  claude: ['claude-agent-acp'],
  'deepseek-harness': ['dsh-acp'],
  'github-copilot': ['copilot', '--acp'],
  junie: ['junie', '--acp=true'],
  kimi: ['kimi', 'acp'],
  opencode: ['opencode', 'acp'],
  pi: ['pi-acp'],
  qwen: ['qwen', '--acp', '--experimental-skills'],
  cline: ['cline', '--acp'],
  cursor: ['cursor-agent', 'acp'],
  glm: ['glm-acp-agent'],
  grok: ['grok', 'agent', 'stdio'],
  kilo: ['kilo', 'acp'],
  'mistral-vibe': ['vibe-acp'],
};

async function manifest() {
  return JSON.parse(await readFile(manifestUrl, 'utf8'));
}

function packageName(spec) {
  return spec.slice(0, spec.lastIndexOf('@'));
}

test('the checked-in runner manifest is complete and produces exact build arguments', async () => {
  const input = await manifest();
  validateRunnerVersions(input);
  const matrix = runnerMatrix(input);

  assert.deepEqual(matrix.map((runner) => runner.id), expectedRunnerIds);
  for (const runner of matrix) {
    const buildArgs = Object.fromEntries(
      runner.build_args.split('\n').map((line) => line.split(/=(.*)/s, 2)),
    );
    assert.match(runner.build_args, new RegExp(`(^|\\n)RUNNER_VERSION=${runner.version}$`));
    assert.match(buildArgs.ACP_SMOKE_COMMAND_B64, /^[A-Za-z0-9+/]+={0,2}$/);
    assert.deepEqual(
      JSON.parse(Buffer.from(buildArgs.ACP_SMOKE_COMMAND_B64, 'base64').toString('utf8')),
      expectedAcpCommands[runner.id],
    );
    assert.doesNotMatch(runner.build_args, /(^|[=@])latest($|\n)/);
  }
  assert.deepEqual(
    Object.fromEntries(Object.entries(input.runners).map(([id, runner]) => [id, runner.acp_command])),
    expectedAcpCommands,
  );
});

test('image and release workflows resolve the same immutable runner revision', async () => {
  const [harnessWorkflow, releaseWorkflow] = await Promise.all([
    readFile(harnessWorkflowUrl, 'utf8'),
    readFile(releaseWorkflowUrl, 'utf8'),
  ]);

  assert.match(harnessWorkflow, /runner_tag=\$\(bash scripts\/runner-revision\.sh\)/);
  assert.match(
    harnessWorkflow,
    /xpressclaw-runner-\$\{\{ matrix\.runner\.id \}\}\$\{\{ matrix\.variant\.suffix \}\}:\$\{\{ needs\.runner-matrix\.outputs\.runner_tag \}\}/,
  );
  assert.doesNotMatch(harnessWorkflow, /xpressclaw-runner-.*:\$\{\{ github\.sha \}\}/);
  assert.match(releaseWorkflow, /RUNNER_TAG=\$\(bash scripts\/runner-revision\.sh\)/);
});

test('updates tracked registry and npm sources while leaving a pinned runner unchanged', async () => {
  const input = await manifest();
  const pinnedClaude = structuredClone(input.runners.claude);
  input.runners.claude.auto_update = false;
  input.runners.claude.pin_reason = 'test pin';
  input.runners['deepseek-harness'].auto_update = true;
  delete input.runners['deepseek-harness'].pin_reason;

  const agents = [];
  for (const runner of Object.values(input.runners)) {
    for (const source of runner.sources) {
      if (source.kind !== 'acp-registry' || agents.some((agent) => agent.id === source.id)) continue;
      const agent = {
        id: source.id,
        version: '9.9.9',
        distribution: {},
      };
      if (source.target === 'package') {
        agent.distribution.npx = {
          package: `${packageName(runner.build_args[source.arg])}@9.9.9`,
        };
      }
      if (source.target === 'binary') {
        const path = runner.build_args.AGENT_PATH;
        agent.distribution.binary = {
          'linux-x86_64': {
            archive: `https://example.test/${source.id}-amd64.tgz`,
            cmd: `./${path}`,
            sha256: 'a'.repeat(64),
          },
          'linux-aarch64': {
            archive: `https://example.test/${source.id}-arm64.tgz`,
            cmd: `./${path}`,
            sha256: 'b'.repeat(64),
          },
        };
      }
      agents.push(agent);
    }
  }

  const updated = await updateRunnerVersions(input, {
    registry: { agents },
    npmVersion: async () => '8.8.8',
  });

  assert.equal(updated.runners.codex.version, '9.9.9');
  assert.equal(updated.runners.codex.build_args.CODEX_ACP_VERSION, '9.9.9');
  const restoredClaude = { ...updated.runners.claude, auto_update: true };
  delete restoredClaude.pin_reason;
  assert.deepEqual(restoredClaude, pinnedClaude);
  assert.equal(
    updated.runners['deepseek-harness'].build_args.AGENT_PACKAGE,
    '@openma/deepseek-harness-acp@8.8.8',
  );
  assert.equal(updated.runners.pi.build_args.AGENT_EXTRA_PACKAGES, '@earendil-works/pi-coding-agent@8.8.8');
  assert.equal(updated.runners.pi.build_args.PI_MCP_PACKAGE, 'pi-mcp-adapter@8.8.8');
  assert.equal(
    updated.runners.cursor.build_args.AGENT_ARCHIVE_AMD64,
    'https://example.test/cursor-amd64.tgz',
  );
  assert.equal(updated.runners.cursor.build_args.AGENT_SHA256_ARM64, 'b'.repeat(64));
});

test('rejects an unexpected binary layout instead of publishing a broken image', async () => {
  const input = await manifest();
  for (const runner of Object.values(input.runners)) {
    runner.auto_update = false;
    runner.pin_reason = 'test pin';
  }
  input.runners.cursor.auto_update = true;
  delete input.runners.cursor.pin_reason;

  await assert.rejects(
    updateRunnerVersions(input, {
      registry: {
        agents: [
          {
            id: 'cursor',
            version: '9.9.9',
            distribution: {
              binary: {
                'linux-x86_64': { archive: 'https://example.test/amd64.tgz', cmd: './moved/cursor' },
                'linux-aarch64': { archive: 'https://example.test/arm64.tgz', cmd: './moved/cursor' },
              },
            },
          },
        ],
      },
      npmVersion: async () => '8.8.8',
    }),
    /registry command changed/,
  );
});

test('rejects an unexpected registry package identity', async () => {
  const input = await manifest();
  for (const runner of Object.values(input.runners)) {
    runner.auto_update = false;
    runner.pin_reason = 'test pin';
  }
  input.runners['github-copilot'].auto_update = true;
  delete input.runners['github-copilot'].pin_reason;

  await assert.rejects(
    updateRunnerVersions(input, {
      registry: {
        agents: [
          {
            id: 'github-copilot-cli',
            version: '9.9.9',
            distribution: { npx: { package: '@example/other-agent@9.9.9' } },
          },
        ],
      },
      npmVersion: async () => '8.8.8',
    }),
    /source changed package identity/,
  );
});
