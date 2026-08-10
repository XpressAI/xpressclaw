import assert from 'node:assert/strict';
import test from 'node:test';

import {
  collectReviewThreads,
  commandResultIsError,
  executeCommandWithReviewLifecycle,
  managedCommandArguments,
  pullRequestUrl,
  TOOL_DESCRIPTION,
  toolDescription,
  updatePullRequestRegistration,
} from './mcp-github.mjs';

test('paginates every pull-request review thread', async () => {
  const cursors = [];
  const threads = await collectReviewThreads(async (after) => {
    cursors.push(after);
    if (after === undefined) {
      return {
        data: {
          repository: {
            pullRequest: {
              reviewThreads: {
                nodes: [{ id: 'thread-1' }],
                pageInfo: { hasNextPage: true, endCursor: 'cursor-1' },
              },
            },
          },
        },
      };
    }
    return {
      data: {
        repository: {
          pullRequest: {
            reviewThreads: {
              nodes: [{ id: 'thread-2' }],
              pageInfo: { hasNextPage: false, endCursor: null },
            },
          },
        },
      },
    };
  });

  assert.deepEqual(cursors, [undefined, 'cursor-1']);
  assert.deepEqual(threads, [{ id: 'thread-1' }, { id: 'thread-2' }]);
});

test('rejects repeated pull-request review-thread cursors', async () => {
  await assert.rejects(
    collectReviewThreads(async () => ({
      data: {
        repository: {
          pullRequest: {
            reviewThreads: {
              nodes: [],
              pageInfo: { hasNextPage: true, endCursor: 'same-cursor' },
            },
          },
        },
      },
    })),
    /invalid or repeated review-thread page cursor/,
  );
});

test('advertises the GitHub MCP tool as the configured replacement for shell gh', () => {
  assert.match(TOOL_DESCRIPTION, /authenticated, project-scoped replacement/);
  assert.match(TOOL_DESCRIPTION, /shell `gh` binary is intentionally unavailable/);
  assert.match(TOOL_DESCRIPTION, /Whenever instructions or skills require `gh`, call this tool/);
  assert.match(TOOL_DESCRIPTION, /repository is fixed by XpressClaw/i);
});

test('ordinary task lifecycle overrides generic draft defaults', () => {
  const environment = { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' };
  assert.deepEqual(
    managedCommandArguments(['pr', 'create', '--draft', '--title', 'Ready'], environment),
    ['pr', 'create', '--title', 'Ready'],
  );
  assert.deepEqual(
    managedCommandArguments(
      [
        'pr', 'create', '--draft=1', '-d=1', '-df', '-fd', '-wd',
        '--title', '-draft remains a title', '-tdrafted-inline',
      ],
      environment,
    ),
    [
      'pr', 'create', '-f', '-f', '-w',
      '--title', '-draft remains a title', '-tdrafted-inline',
    ],
  );
  assert.match(toolDescription(environment), /published ready for review, never left as a draft/);
  assert.match(toolDescription(environment), /only after approval or merge/);
});

test('ordinary task lifecycle rejects converting a ready PR back to draft', () => {
  const environment = { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' };
  assert.throws(
    () => managedCommandArguments(['pr', 'ready', '--undo'], environment),
    /cannot convert a ready pull request back to draft/,
  );
  assert.deepEqual(
    managedCommandArguments(['pr', 'ready', '--undo'], {}),
    ['pr', 'ready', '--undo'],
  );
});

test('ordinary task lifecycle rejects dry-run creation before publication', () => {
  const environment = { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' };
  assert.throws(
    () => managedCommandArguments(['pr', 'create', '--dry-run'], environment),
    /cannot register a dry-run pull request/,
  );
  assert.deepEqual(
    managedCommandArguments(['pr', 'create', '--dry-run'], {}),
    ['pr', 'create', '--dry-run'],
  );
});

test('workflow-managed PR creation preserves an explicit draft', () => {
  assert.deepEqual(
    managedCommandArguments(['pr', 'create', '--draft'], {}),
    ['pr', 'create', '--draft'],
  );
  assert.doesNotMatch(toolDescription({}), /only after approval or merge/);
});

test('extracts a pull request URL from human or JSON output', () => {
  assert.equal(
    pullRequestUrl('Created https://github.com/XpressAI/xpressclaw/pull/151\n'),
    'https://github.com/XpressAI/xpressclaw/pull/151',
  );
  assert.equal(pullRequestUrl('no pull request'), undefined);
});

test('fails closed when review-lifecycle registration fails', () => {
  assert.equal(commandResultIsError({ exit_code: 0 }), false);
  assert.equal(commandResultIsError({ exit_code: 1 }), true);
  assert.equal(commandResultIsError({
    exit_code: 0,
    review_lifecycle: { registered: false },
  }), true);
});

test('sends explicit begin, register, and cancel registration phases', async () => {
  const requests = [];
  const environment = {
    XPRESSCLAW_URL: 'http://control-plane/',
    XPRESSCLAW_TASK_ID: 'task/1',
    XPRESSCLAW_AGENT_ID: 'agent',
  };
  const fetchImplementation = async (url, options) => {
    requests.push({ url, body: JSON.parse(options.body) });
    return { ok: true, status: 200, text: async () => '{"status":"ok"}' };
  };

  await updatePullRequestRegistration(
    'begin', undefined, 'registration-1', environment, fetchImplementation,
  );
  await updatePullRequestRegistration(
    'register',
    'https://github.com/XpressAI/xpressclaw/pull/151',
    'registration-1',
    environment,
    fetchImplementation,
  );
  await updatePullRequestRegistration(
    'cancel', undefined, 'registration-1', environment, fetchImplementation,
  );

  assert.deepEqual(requests, [
    {
      url: 'http://control-plane/api/tasks/task%2F1/pull-requests',
      body: {
        phase: 'begin', agent_id: 'agent', registration_id: 'registration-1',
      },
    },
    {
      url: 'http://control-plane/api/tasks/task%2F1/pull-requests',
      body: {
        phase: 'register',
        agent_id: 'agent',
        registration_id: 'registration-1',
        url: 'https://github.com/XpressAI/xpressclaw/pull/151',
      },
    },
    {
      url: 'http://control-plane/api/tasks/task%2F1/pull-requests',
      body: {
        phase: 'cancel', agent_id: 'agent', registration_id: 'registration-1',
      },
    },
  ]);
});

test('arms review monitoring before publishing and leaves the gate on registration failure', async () => {
  const events = [];
  const output = await executeCommandWithReviewLifecycle(
    ['pr', 'create', '--title', 'Ready'],
    {
      environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
      registrationId: 'registration-1',
      execute: async () => {
        events.push('command');
        return {
          exit_code: 0,
          stdout: 'https://github.com/XpressAI/xpressclaw/pull/151\n',
          stderr: '',
        };
      },
      updateRegistration: async (phase, _url, registrationId) => {
        events.push(`${phase}:${registrationId}`);
        if (phase === 'register') throw new Error('control plane timed out');
        return { status: 'ok' };
      },
    },
  );

  assert.deepEqual(events, ['begin:registration-1', 'command', 'register:registration-1']);
  assert.equal(output.review_lifecycle.registered, false);
  assert.match(output.review_lifecycle.message, /durable task gate remains armed/i);
  assert.equal(commandResultIsError(output), true);
});

test('does not publish unless the durable review gate was armed', async () => {
  let executed = false;
  const output = await executeCommandWithReviewLifecycle(['pr', 'create'], {
    environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
    registrationId: 'registration-1',
    execute: async () => {
      executed = true;
      return { exit_code: 0, stdout: '', stderr: '' };
    },
    updateRegistration: async () => {
      throw new Error('database unavailable');
    },
  });

  assert.equal(executed, false);
  assert.equal(output.review_lifecycle.registered, false);
  assert.match(output.review_lifecycle.message, /command was not run/i);
});

test('cancels the pre-publication gate when the GitHub command fails', async () => {
  const events = [];
  const output = await executeCommandWithReviewLifecycle(['pr', 'ready'], {
    environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
    registrationId: 'registration-1',
    execute: async () => {
      events.push('command');
      return { exit_code: 1, stdout: '', stderr: 'not found' };
    },
    updateRegistration: async (phase, _url, registrationId) => {
      events.push(`${phase}:${registrationId}`);
      return { status: 'ok' };
    },
  });

  assert.deepEqual(events, ['begin:registration-1', 'command', 'cancel:registration-1']);
  assert.equal(output.exit_code, 1);
  assert.equal(output.review_lifecycle, undefined);
});

test('resolves the explicit pull request target after ready output omits its URL', async () => {
  const resolvedTargets = [];
  const output = await executeCommandWithReviewLifecycle(
    ['pr', 'ready', 'feature/review-lifecycle'],
    {
      environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
      registrationId: 'registration-1',
      execute: async () => ({
        exit_code: 0,
        stdout: '✓ Pull request XpressAI/xpressclaw#151 is marked as ready for review\n',
        stderr: '',
      }),
      currentPullRequestUrl: async (target) => {
        resolvedTargets.push(target);
        return 'https://github.com/XpressAI/xpressclaw/pull/151';
      },
      updateRegistration: async () => ({ status: 'waiting' }),
    },
  );

  assert.deepEqual(resolvedTargets, ['feature/review-lifecycle']);
  assert.equal(
    output.review_lifecycle.pull_request,
    'https://github.com/XpressAI/xpressclaw/pull/151',
  );
});
