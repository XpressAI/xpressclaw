import assert from 'node:assert/strict';
import test from 'node:test';

import {
  commandResultIsError,
  managedCommandArguments,
  pullRequestUrl,
  TOOL_DESCRIPTION,
  toolDescription,
} from './mcp-github.mjs';

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
  assert.match(toolDescription(environment), /published ready for review, never left as a draft/);
  assert.match(toolDescription(environment), /only after approval or merge/);
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
