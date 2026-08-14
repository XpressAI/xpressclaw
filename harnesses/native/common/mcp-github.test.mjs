import assert from 'node:assert/strict';
import test from 'node:test';

import {
  collectReviewThreadComments,
  collectReviewThreads,
  commandResultIsError,
  executeCommandWithReviewLifecycle,
  managedCommandArguments,
  pullRequestRegistrationKey,
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

test('paginates comments within every pull-request review thread', async () => {
  const pages = [];
  const threads = await collectReviewThreadComments(
    [
      {
        id: 'thread-1',
        comments: {
          nodes: [{ id: 'comment-1' }],
          pageInfo: { hasNextPage: true, endCursor: 'comment-cursor-1' },
        },
      },
      {
        id: 'thread-2',
        comments: {
          nodes: [{ id: 'comment-3' }],
          pageInfo: { hasNextPage: false, endCursor: null },
        },
      },
    ],
    async (threadId, after) => {
      pages.push([threadId, after]);
      return {
        data: {
          node: {
            comments: {
              nodes: [{ id: 'comment-2' }],
              pageInfo: { hasNextPage: false, endCursor: null },
            },
          },
        },
      };
    },
  );

  assert.deepEqual(pages, [['thread-1', 'comment-cursor-1']]);
  assert.deepEqual(threads[0].comments.nodes, [
    { id: 'comment-1' },
    { id: 'comment-2' },
  ]);
  assert.deepEqual(threads[1].comments.nodes, [{ id: 'comment-3' }]);
});

test('rejects repeated pull-request review-thread comment cursors', async () => {
  await assert.rejects(
    collectReviewThreadComments(
      [{
        id: 'thread-1',
        comments: {
          nodes: [],
          pageInfo: { hasNextPage: true, endCursor: 'same-cursor' },
        },
      }],
      async () => ({
        data: {
          node: {
            comments: {
              nodes: [],
              pageInfo: { hasNextPage: true, endCursor: 'same-cursor' },
            },
          },
        },
      }),
    ),
    /invalid or repeated review-thread comment page cursor/,
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
  assert.deepEqual(
    managedCommandArguments(['pr', 'ready', '--undo=false'], environment),
    ['pr', 'ready', '--undo=false'],
  );
  for (const value of ['0', 'f', 'F', 'FALSE', 'False']) {
    assert.deepEqual(
      managedCommandArguments(['pr', 'ready', `--undo=${value}`], environment),
      ['pr', 'ready', `--undo=${value}`],
    );
  }
  assert.throws(
    () => managedCommandArguments(['pr', 'ready', '--undo=true'], environment),
    /cannot convert a ready pull request back to draft/,
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
  assert.deepEqual(
    managedCommandArguments(['pr', 'create', '--dry-run=false'], environment),
    ['pr', 'create', '--dry-run=false'],
  );
  for (const value of ['0', 'f', 'F', 'FALSE', 'False']) {
    assert.deepEqual(
      managedCommandArguments(['pr', 'create', `--dry-run=${value}`], environment),
      ['pr', 'create', `--dry-run=${value}`],
    );
  }
  assert.throws(
    () => managedCommandArguments(['pr', 'create', '--dry-run=true'], environment),
    /cannot register a dry-run pull request/,
  );
});

test('uses the same registration key for create and ready on one pull request', async () => {
  const environment = { GH_REPO: 'XpressAI/xpressclaw' };
  const created = await pullRequestRegistrationKey(
    ['pr', 'create', '--head', 'XpressAI:feature/review', '--base=main'],
    { environment },
  );
  const readied = await pullRequestRegistrationKey(
    ['pr', 'ready', '151'],
    {
      environment,
      currentPullRequestIdentity: async () => ({
        owner: 'xpressai', head: 'feature/review', base: 'main',
      }),
    },
  );

  assert.equal(created, readied);
  assert.match(created, /^[0-9a-f]{64}$/);
});

test('registration keys distinguish unrelated pull-request branches', async () => {
  const environment = { GH_REPO: 'XpressAI/xpressclaw' };
  const first = await pullRequestRegistrationKey(
    ['pr', 'create', '-Hfeature/one', '-Bmain'],
    { environment },
  );
  const second = await pullRequestRegistrationKey(
    ['pr', 'create', '-Hfeature/two', '-Bmain'],
    { environment },
  );

  assert.notEqual(first, second);
});

test('registration keys normalize equals-attached short option values', async () => {
  const environment = { GH_REPO: 'XpressAI/xpressclaw' };
  const created = await pullRequestRegistrationKey(
    ['pr', 'create', '-H=feature/review', '-B=main'],
    { environment },
  );
  const readied = await pullRequestRegistrationKey(
    ['pr', 'ready', '151'],
    {
      environment,
      currentPullRequestIdentity: async () => ({
        owner: 'xpressai', head: 'feature/review', base: 'main',
      }),
    },
  );

  assert.equal(created, readied);
});

test('registration keys parse value options bundled after Boolean shorthands', async () => {
  const environment = { GH_REPO: 'XpressAI/xpressclaw' };
  const created = await pullRequestRegistrationKey(
    ['pr', 'create', '-fHfeature/review', '-fBmain'],
    { environment },
  );
  const readied = await pullRequestRegistrationKey(
    ['pr', 'ready', '151'],
    {
      environment,
      currentPullRequestIdentity: async () => ({
        owner: 'xpressai', head: 'feature/review', base: 'main',
      }),
    },
  );

  assert.equal(created, readied);
});

test('registration keys honor the branch-configured gh merge base', async () => {
  const environment = { GH_REPO: 'XpressAI/xpressclaw' };
  const created = await pullRequestRegistrationKey(
    ['pr', 'create', '--head=feature/review'],
    {
      environment,
      configuredBaseBranch: async () => 'develop',
      defaultBaseBranch: async () => 'main',
    },
  );
  const readied = await pullRequestRegistrationKey(
    ['pr', 'ready', '151'],
    {
      environment,
      currentPullRequestIdentity: async () => ({
        owner: 'xpressai', head: 'feature/review', base: 'develop',
      }),
    },
  );

  assert.equal(created, readied);
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
  const headers = [];
  const environment = {
    XPRESSCLAW_URL: 'http://control-plane/',
    XPRESSCLAW_CONTROL_TOKEN: 'internal-secret',
    XPRESSCLAW_TASK_ID: 'task/1',
    XPRESSCLAW_AGENT_ID: 'agent',
  };
  const fetchImplementation = async (url, options) => {
    requests.push({ url, body: JSON.parse(options.body) });
    headers.push(options.headers);
    return { ok: true, status: 200, text: async () => '{"status":"ok"}' };
  };

  await updatePullRequestRegistration(
    'begin', undefined, 'registration-1', 'a'.repeat(64), environment, fetchImplementation,
  );
  await updatePullRequestRegistration(
    'register',
    'https://github.com/XpressAI/xpressclaw/pull/151',
    'registration-1',
    'a'.repeat(64),
    environment,
    fetchImplementation,
  );
  await updatePullRequestRegistration(
    'cancel', undefined, 'registration-1', 'a'.repeat(64), environment, fetchImplementation,
  );

  assert.deepEqual(requests, [
    {
      url: 'http://control-plane/api/tasks/task%2F1/pull-requests',
      body: {
        phase: 'begin', agent_id: 'agent', registration_id: 'registration-1',
        registration_key: 'a'.repeat(64),
      },
    },
    {
      url: 'http://control-plane/api/tasks/task%2F1/pull-requests',
      body: {
        phase: 'register',
        agent_id: 'agent',
        registration_id: 'registration-1',
        registration_key: 'a'.repeat(64),
        url: 'https://github.com/XpressAI/xpressclaw/pull/151',
      },
    },
    {
      url: 'http://control-plane/api/tasks/task%2F1/pull-requests',
      body: {
        phase: 'cancel', agent_id: 'agent', registration_id: 'registration-1',
        registration_key: 'a'.repeat(64),
      },
    },
  ]);
  assert.equal(headers.length, 3);
  assert.ok(headers.every((value) => value['x-xpressclaw-internal-token'] === 'internal-secret'));
});

test('arms review monitoring before publishing and leaves the gate on registration failure', async () => {
  const events = [];
  const output = await executeCommandWithReviewLifecycle(
    ['pr', 'create', '--title', 'Ready'],
    {
      environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
      registrationId: 'registration-1',
      registrationKeyForCommand: async () => 'a'.repeat(64),
      execute: async () => {
        events.push('command');
        return {
          exit_code: 0,
          stdout: 'https://github.com/XpressAI/xpressclaw/pull/151\n',
          stderr: '',
        };
      },
      updateRegistration: async (phase, _url, registrationId, registrationKey) => {
        events.push(`${phase}:${registrationId}:${registrationKey}`);
        if (phase === 'register') throw new Error('control plane timed out');
        return { status: 'ok' };
      },
    },
  );

  assert.deepEqual(events, [
    `begin:registration-1:${'a'.repeat(64)}`,
    'command',
    `register:registration-1:${'a'.repeat(64)}`,
  ]);
  assert.equal(output.review_lifecycle.registered, false);
  assert.match(output.review_lifecycle.message, /durable task gate remains armed/i);
  assert.equal(commandResultIsError(output), true);
});

test('does not publish unless the durable review gate was armed', async () => {
  let executed = false;
  const output = await executeCommandWithReviewLifecycle(['pr', 'create'], {
    environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
    registrationId: 'registration-1',
    registrationKeyForCommand: async () => 'a'.repeat(64),
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
    registrationKeyForCommand: async () => 'a'.repeat(64),
    execute: async () => {
      events.push('command');
      return { exit_code: 1, stdout: '', stderr: 'not found' };
    },
    updateRegistration: async (phase, _url, registrationId, registrationKey) => {
      events.push(`${phase}:${registrationId}:${registrationKey}`);
      return { status: 'ok' };
    },
  });

  assert.deepEqual(events, [
    `begin:registration-1:${'a'.repeat(64)}`,
    'command',
    `cancel:registration-1:${'a'.repeat(64)}`,
  ]);
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
      registrationKeyForCommand: async () => 'a'.repeat(64),
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

test('a retry reuses the durable registration token returned by begin', async () => {
  const events = [];
  const output = await executeCommandWithReviewLifecycle(['pr', 'ready', '151'], {
    environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
    registrationId: 'registration-new',
    registrationKeyForCommand: async () => 'a'.repeat(64),
    execute: async () => ({
      exit_code: 0,
      stdout: 'https://github.com/XpressAI/xpressclaw/pull/151\n',
      stderr: '',
    }),
    updateRegistration: async (phase, _url, registrationId) => {
      events.push(`${phase}:${registrationId}`);
      if (phase === 'begin') {
        return { registration_id: 'registration-original', reused: true };
      }
      return { status: 'waiting' };
    },
  });

  assert.deepEqual(events, [
    'begin:registration-new',
    'register:registration-original',
  ]);
  assert.equal(output.review_lifecycle.registered, true);
});

test('a retry registers a pull request that gh reports is already ready', async () => {
  const events = [];
  const resolvedTargets = [];
  const output = await executeCommandWithReviewLifecycle(['pr', 'ready', '151'], {
    environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
    registrationId: 'registration-new',
    registrationKeyForCommand: async () => 'a'.repeat(64),
    execute: async () => ({
      exit_code: 1,
      stdout: '',
      stderr: 'Pull request XpressAI/xpressclaw#151 is already ready for review',
    }),
    currentPullRequestUrl: async (target) => {
      resolvedTargets.push(target);
      return 'https://github.com/XpressAI/xpressclaw/pull/151';
    },
    updateRegistration: async (phase, _url, registrationId) => {
      events.push(`${phase}:${registrationId}`);
      if (phase === 'begin') {
        return { registration_id: 'registration-original', reused: true };
      }
      return { status: 'waiting' };
    },
  });

  assert.deepEqual(events, [
    'begin:registration-new',
    'register:registration-original',
  ]);
  assert.deepEqual(resolvedTargets, ['151']);
  assert.equal(output.exit_code, 0);
  assert.equal(output.review_lifecycle.registered, true);
});

test('a failed retry preserves the original durable registration gate', async () => {
  const events = [];
  await executeCommandWithReviewLifecycle(['pr', 'ready', '151'], {
    environment: { XPRESSCLAW_GITHUB_REVIEW_LIFECYCLE: '1' },
    registrationId: 'registration-new',
    registrationKeyForCommand: async () => 'a'.repeat(64),
    execute: async () => ({ exit_code: 1, stdout: '', stderr: 'not ready' }),
    updateRegistration: async (phase, _url, registrationId) => {
      events.push(`${phase}:${registrationId}`);
      return phase === 'begin'
        ? { registration_id: 'registration-original', reused: true }
        : { status: 'ok' };
    },
  });

  assert.deepEqual(events, ['begin:registration-new']);
});
