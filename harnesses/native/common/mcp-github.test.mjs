import assert from 'node:assert/strict';
import test from 'node:test';

import { TOOL_DESCRIPTION } from './mcp-github.mjs';

test('advertises the GitHub MCP tool as the configured replacement for shell gh', () => {
  assert.match(TOOL_DESCRIPTION, /authenticated, project-scoped replacement/);
  assert.match(TOOL_DESCRIPTION, /shell `gh` binary is intentionally unavailable/);
  assert.match(TOOL_DESCRIPTION, /Whenever instructions or skills require `gh`, call this tool/);
  assert.match(TOOL_DESCRIPTION, /repository is fixed by XpressClaw/i);
});
