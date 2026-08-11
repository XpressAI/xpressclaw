# ADR-033: Retained Agent Runtimes

## Status

Accepted

## Context

ADR-025 and ADR-026 made every ACP attempt a disposable container. The host
workspace and selected harness authentication directories survived, but the
container's writable layer did not. As a result, SDKs, user-level tools,
caches, browser downloads, `/home/node`, and `/tmp` disappeared after every
turn. Agents repeatedly rediscovered and rebuilt the same environment, which
wasted time and model context and made follow-up turns observe a different
machine from the one they had just configured.

Restarting only the ACP process still repeats its handshake, adapter startup,
authentication discovery, MCP startup, and in-memory session setup. ACP is a
multi-turn protocol, so those costs are not an inherent turn boundary.

## Decision

Each configured Agent owns one retained container whose engine name is
a deterministic SHA-256 encoding of the installation and logical Agent IDs.
This keeps names within Docker's ASCII grammar even when an Agent is named in
Japanese or another non-ASCII script, while labels retain the human-readable
logical ID. It is allocated lazily by the first task. XpressClaw starts and
initializes the ACP command once, then keeps that process and its stdio
connection alive across ordinary turns. The same process may own several ACP
sessions: a follow-up uses its already-live task session, while new work creates
or forks a session through the same connection. The container's writable layer,
including `/home/node` and `/tmp`, remains available throughout.

ACP session identifiers are persisted before each prompt is dispatched. If the
process or control plane crashes, the next task restarts the retained container
and resumes or loads the persisted session. A hard cancellation, runner
configuration or image change, project deletion, and control-plane shutdown are
also explicit process boundaries. Normal successful turns are not.

Agent task-queue dispatch remains serialized. A running queue row is also the
lease on the shared process, including the short cancellation interval after an
attempt becomes terminal but before a hard stop has completed. This prevents a
new turn from entering a process that an older cancellation path is stopping.

Retained containers carry lifecycle, installation-owner, logical-Agent, and
runner-specification labels. The installation ID is generated once and stored
with the database. Container enumeration ignores resources labelled for another
installation, so two control planes can safely share an engine. The
specification fingerprint covers the image reference, command, environment,
mounts, limits, working directory, and network configuration. XpressClaw
recreates the container when that fingerprint changes or its image no longer
matches the current local image. The fingerprint is SHA-256 so secrets in the
environment are not written directly to container labels or logs.

Configured startup commands initialize a retained environment once. A marker is
written only after every command succeeds; a failed initialization is retried
on the next process start. Changing a startup command changes the container
specification and therefore creates a clean environment and process.

Control-plane shutdown and startup stop configured Agent containers but
retain them. Deleting an Agent removes its container and writable layer.
Startup cleanup operates only on containers carrying this installation's owner
label. Containers owned by another installation—and unlabelled legacy
containers whose ownership cannot be proven—are never included in that cleanup.

## Consequences

- Follow-up turns reuse the initialized ACP adapter, MCP processes, live
  sessions, tools, caches, and files instead of rebuilding them.
- An initialized idle project consumes the runner process's baseline memory in
  addition to disk. It should consume negligible CPU while waiting on stdio.
  Projects that have never run a task allocate no container.
- Updating runner mounts, credentials, environment, image, or command creates
  a clean container. Workspace and explicitly mounted host or named-volume
  data remain governed by those mounts.
- Project deletion is destructive for unmounted container state. The existing
  deletion confirmation is therefore the user-visible cleanup boundary.
- ACP sessions remain durable independently of the live process. Persistence
  and `session/resume`, `session/load`, or `session/fork` provide recovery after
  the unavoidable restart boundaries above.
