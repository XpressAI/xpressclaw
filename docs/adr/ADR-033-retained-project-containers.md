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
logical ID. It is allocated lazily by the first task. XpressClaw initializes ACP
once and keeps that process and its stdio connection alive across ordinary turns.
The same process may own several ACP sessions: a follow-up uses its already-live
task session, while new work creates or forks a session through the same
connection. The container's writable layer,
including `/home/node` and `/tmp`, remains available throughout.

ACP session identifiers are persisted before each prompt is dispatched. If the
process or control plane crashes, the next task restarts the retained container
and resumes or loads the persisted session. Hard cancellation, explicit
environment recovery or rebuild, project deletion, and control-plane shutdown are process
boundaries. Normal successful turns and available image updates are not.

Agent task-queue dispatch remains serialized. A running queue row is also the
lease on the shared process, including the short cancellation interval after an
attempt becomes terminal but before a hard stop has completed. This prevents a
new turn from entering a process that an older cancellation path is stopping.

Retained containers carry lifecycle, installation-owner, logical-Agent, and
runner-specification labels. The installation ID is generated once and stored
with the database. Container enumeration ignores resources labelled for another
installation, so two control planes can safely share an engine. The
specification fingerprint covers the image reference, command, environment,
mounts, limits, working directory, and network configuration. XpressClaw preserves
the existing environment even when the selected image or its local tag changes. Capability checks use the existing container's immutable
image ID, not a newer tag. Reuse validates the original specification with the
container's original image reference; mounts, credentials, command, working
directory, user identity, resource limits, and networking must still match.
The fingerprint is SHA-256 so secrets are not written to labels or logs.

Changed launch settings trigger automatic recovery on the next Task or
Conversation turn. Recovery takes the exclusive Agent runtime boundary and
waits for other turns to finish. It checks ownership and container dependencies
before stopping anything, snapshots the filesystem, and starts a replacement
with the saved settings. It retains the existing base image, installed tools,
unmounted files, and native session storage. Mounted data is not included in a
snapshot; configured mounts and retained anonymous image volumes are reattached.
Old environment overrides are cleared before applying the current values, so
rotated or removed credentials are not silently inherited from the snapshot.

The old container remains under a deterministic recovery name until the new ACP
process initializes successfully. Failed creation, startup, or initialization
restores the old container. An interrupted recovery is detected and rolled back
before retrying on the next turn. Ownership labels bind each replacement to its
backup; unknown containers are never removed. Explicit rebuild and deletion
include owned recovery backups in their dependency preflight and cleanup.
Snapshots require local disk space; snapshot failure preserves the original.
After removing a recovered container, clean up its unused snapshot and any
superseded snapshot parents. Cleanup never forces image removal or prunes the
original base image. Preserved helper environments retain their snapshots;
images with user-added tags remain under user control.

Downloading a harness update only pins the selected image for a future environment.
It needs no idle runtime boundary and does not hold the configuration lock during
registry access. Concurrent changes to the selected harness are rechecked before
pinning the downloaded digest; unrelated configuration changes are retained.
Applying it to an existing environment requires a separate, confirmed rebuild.
Rebuild requires no running work under the runtime lifecycle barrier, and warns
that installed tools and unmounted files will be lost. It removes the old
environment; the next turn creates one using the saved settings. No automatic
update is allowed to discard a retained writable layer.
After successful removal, clear the Agent's persisted task and conversation
native session IDs while still holding the runtime boundary. Their harness
storage may have lived in the deleted layer. Preserve logical history and start
fresh harness sessions on subsequent turns. A rejected rebuild leaves IDs intact.
Queued tasks and Conversation turns do not prevent maintenance; they use the
updated environment when dispatched. Running queue leases still prevent it,
including the cleanup interval after an attempt reaches a terminal state.

Before recovery or explicit removal, inspect all containers (including stopped and
unlabelled containers) for namespace, volume-source, and link dependencies. Running
helpers block replacement with their names and an instruction to stop them. Stopped
helpers bound to the parent ID no longer block recovery or rebuild: preserve the
old parent under `<canonical>-preserved-<full-container-id>` and let the Agent use a
replacement. Keep the helper containers and all their files unchanged. Recovery
archives its backup only after the replacement ACP handshake succeeds; failures
still roll back to the original container. Preserved parents stay stopped and
helpers are not automatically restarted. Restarting those helpers later requires
starting their preserved parent first. Name-based references cannot safely follow
a renamed parent and are reported explicitly instead of silently repointed.

Harness settings list preserved environments and their dependent helpers, including
after a server restart. Archives retain disk space and are never automatically
pruned. Subsequent rebuilds affect only the current environment and any unfinished
recovery. Agent deletion preflights archives too and remains blocked by dependencies.
Never recursively remove helper containers. Engine inspection failures fail closed;
check every candidate before any mutation. The engine remains authoritative if a
trusted host-engine client changes dependencies during removal. Host-engine access
remains opt-in as specified in ADR-027.

Dispatchers register cancellable workers by durable queue/turn ID. Before rebuilding
an otherwise idle Agent, cancel only registered workers whose dispatch is no longer
running and wait for their actual runtime guards to drop. Never infer idleness just
from a completed task or forcibly replace a held lock. Running dispatches remain
protected, and unrelated Agents are unaffected. A bounded wait reports runtime
contention separately from active work; active work errors include task/conversation
names. This repairs a finished dispatch that left a worker holding the runtime
boundary without requiring the user to find or cancel nonexistent active tasks.

Agent deletion must also stop if cleanup of an active legacy attempt container
fails, retaining the Agent's configuration and records for repair and retry.

Configured startup commands initialize a retained environment once. A marker is
written only after every command succeeds; a failed initialization is retried
on the next process start. Changing a startup command changes the container
specification and runs the new initialization during automatic recovery.

Control-plane shutdown and startup stop configured Agent containers but retain
them. Deleting an Agent removes its container and writable layer.
Startup cleanup operates only on containers carrying this installation's owner
label. Containers owned by another installation—and unlabelled legacy
containers whose ownership cannot be proven—are never included in that cleanup.

## Consequences

- Follow-up turns reuse the initialized ACP adapter, MCP processes, live
  sessions, tools, caches, and files instead of rebuilding them.
- An initialized idle project consumes the runner process's baseline memory in
  addition to disk. It should consume negligible CPU while waiting on stdio.
  Projects that have never run a task allocate no container.
- Image updates leave the current environment and installed tools intact. Changed
  launch settings are recovered automatically without discarding installed tools
  or silently reusing stale authority. Workspace and
  explicitly mounted host or named-volume data survive a confirmed rebuild.
- Project deletion is destructive for unmounted container state. The existing
  deletion confirmation is therefore the user-visible cleanup boundary.
- ACP sessions remain durable independently of the live process. Persistence
  and `session/resume`, `session/load`, or `session/fork` provide recovery after
  the unavoidable restart boundaries above.

## Validation

`automatic_recovery_preserves_files_volumes_and_rolls_back_failures` exercises
snapshot recovery, anonymous volume identity, revoked environment credentials,
failed startup, and interrupted recovery against Docker/Podman. Run with
`XPRESSCLAW_RETENTION_TEST_IMAGE=node:22-bookworm-slim` and `--ignored`.

`stopped_helpers_preserve_their_files_without_blocking_recovery_or_rebuild`
reproduces the idle-Agent/stopped-helper case against Docker/Podman. It verifies
rollback, parent/helper writable-layer retention, subsequent fresh rebuilds,
archive discovery, and deletion protection. Worker tests prove that only finished
dispatches release their leases; route tests distinguish runtime contention from
named active work and expose persistent archive inventory.

Dependency preflight tests cover running and stopped unlabelled containers,
namespace/volume/link references, and failed engine inspection. Mixed retained
and legacy environments are checked together so a blocked legacy container
cannot cause partial deletion of the current environment. The optional
`image_update_retains_installed_files_and_dependent_container` integration test
uses isolated containers to verify a writable-layer marker, stable container ID,
and dependent network service across an image-selection change, plus installed-file
retention across an explicit process restart.
Run it with
`XPRESSCLAW_RETENTION_TEST_IMAGE=node:22-bookworm-slim` on Docker or Podman.
The server integration test `rebuild_and_delete_preserve_session_and_container_boundaries`
uses the same fixture to check session invalidation only after successful rebuild
and Agent preservation when a legacy attempt has a dependent container.

Hard cancellation, process recovery, and control-plane restart still stop the
shared Agent container and can disrupt namespace-sharing services. This change
prevents an image update from initiating those lifecycle boundaries. Services
that must survive an Agent restart should use their own network namespace (for
example, an independent named network) instead of the Agent container's namespace.
