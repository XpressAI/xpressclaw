# ADR-041: Active Repository Adoption

## Status

Accepted

## Context

An Agent created without a repository can clone one into a child directory of
its workspace. Previously XpressClaw continued treating the broad workspace as
the Git root: Git status failed, the ACP process kept the wrong working
directory, and the bundled GitHub MCP was not discovered on later turns.
Conflating the writable mount boundary with the current checkout also made an
instance-wide fallback workspace unsafe for newly created blank Agents.

## Decision

Each Agent has two distinct local concepts:

- the configured **bootstrap workspace**, which is the canonical writable
  authorization and mount boundary; and
- an optional durable **active repository**, stored in local SQLite as a path
  relative to that boundary plus an identity derived from its path and origin.

New Agents without an explicit path receive an isolated managed workspace.
Legacy and explicit workspaces are preserved in place. Repository discovery is
bounded to four directory levels, 512 visited directories, and 32 candidates;
it ignores symlinks and common internal/build directories. A root checkout is
deterministic, as is exactly one nested checkout. Multiple candidates remain
ambiguous until a user selects one.

The UI and authenticated workspace API expose candidates and precise GitHub
availability diagnostics. The Agent's constrained XpressClaw control MCP may
propose a canonical path mapped from its container workspace, but cannot write
an arbitrary host path. User selections, clears, and control-tool proposals are
queued and applied only at the next safe turn boundary.

Compatible runners also receive a credential-free bootstrap GitHub MCP before
a repository is active. Its optional `cwd` is canonicalized to a Git root inside
the bootstrap boundary and sent over the per-Agent callback channel. The
control plane fixes the GitHub owner/repository from the origin, obtains the
matching credential, and returns it only to that MCP invocation. A successful
first resolution persists the active path immediately so the current GitHub
command can run, while a same-path boundary marker guarantees that the old-cwd
ACP process and any session ID written at turn completion are discarded before
the next turn. Omitting `cwd` with multiple candidates returns relative paths
and never guesses.

When the selected path or repository origin changes, XpressClaw enters the
per-Agent lifecycle barrier, clears Task and Conversation native session
handles, retires live ACP processes, and replaces the retained container when
needed. The next turn uses the active repository for its cwd, Git metrics,
Files/Git APIs, and bundled GitHub MCP discovery. Durable messages, logical
sessions, and managed pull-request state are not deleted.

Internal control callbacks use a per-Agent HMAC capability derived from the
listener secret. A runner therefore cannot retarget an adoption request or PR
lifecycle callback at another Agent. Discovery and explicit selection both
canonicalize paths, reject traversal and symlink escape, and never leave the
bootstrap boundary.

## Consequences

- The blank-Agent flow is: clone, use GitHub immediately with a validated `cwd`
  or adopt/select at the boundary, then use the persisted default thereafter.
- A missing checkout or changed origin invalidates runtime identity visibly;
  XpressClaw never substitutes an unrelated repository.
- Clearing a selection disables automatic adoption until another explicit
  selection is made.
- Repository selection is instance-local runtime state and is intentionally
  excluded from portable Project configuration and synchronization.
