# ADR-034: Workspace Browser, Editor, and Terminal

## Status

Accepted

## Context

Remote XpressClaw users could follow an agent's task but could not inspect or
correct its workspace without asking the agent to print files or publish a
branch. They also could not complete interactive authentication inside the
agent's retained environment. Tool activity sometimes names edited files, but
that event stream is not a reliable statement of the workspace's current Git
state: users and later tasks can change the same checkout.

## Decision

Each Agent has a **Files** view with three related capabilities:

1. A lazy directory tree and UTF-8 file API rooted at the Agent's configured
   host workspace. The workspace is already mounted read-write into the
   retained Agent container, so host-side reads and saves immediately appear
   to the harness without requiring the container to be running.
2. A lazily loaded Monaco editor with language-aware highlighting and a diff
   view. A read returns a SHA-256 content revision; a save must provide that
   revision and receives `409 Conflict` if another user or agent changed the
   file in the meantime. Saves replace the file atomically and preserve its
   permissions.
3. An xterm.js terminal attached through a WebSocket to a TTY created with
   `docker exec` in the installation-owned retained Agent container. A
   stopped retained container is restarted for the session. An Agent that has
   never run a task explains that one task must initialize its environment
   before a terminal is available.

Git status and per-file staged/working-tree diffs are read directly from the
configured workspace. Task details show the current changed files and link
into the Agent's Files view. The UI deliberately calls these **workspace
changes**, not task changes, because Git cannot prove which task produced an
uncommitted edit.

## Security and limits

Workspace and terminal endpoints are intentionally equivalent to local project
access. They therefore:

- accept browser requests only when `Origin` matches `Host`, or when the
  browser reports `Sec-Fetch-Site: same-origin` through a reverse proxy that
  rewrites `Host`; origin-less calls remain available to the desktop shell and
  trusted API clients;
- resolve only normalized relative paths through a capability directory handle
  rooted at the workspace. Reads, listings, temporary saves, and atomic renames
  stay relative to that handle, so a container cannot redirect a checked path
  outside the workspace by swapping an ancestor for a symlink;
- edit existing regular UTF-8 files only, with a 4 MiB file limit;
- cap directory listings at 2,000 entries and displayed Git output at 4 MiB;
- attach terminals only to retained containers bearing this installation and
  Agent's ownership labels.

Anyone allowed to reach an XpressClaw instance can already direct its agents.
Deployment authentication and network isolation remain the outer security
boundary; the same-origin check is defense against a malicious unrelated web
page driving these APIs through the user's browser, not a replacement for that
boundary.

## Consequences

- Users can inspect, edit, compare, and authenticate a remote Agent without a
  GitHub round trip.
- Monaco and xterm are split into lazy browser chunks, so ordinary task and
  Agent pages do not pay their startup cost.
- Concurrent edits fail visibly instead of silently overwriting agent work.
- Binary files, new-file creation, rename/delete actions, search, and a full
  source-control UI remain future work.
