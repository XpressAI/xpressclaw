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
environment rebuild, project deletion, and control-plane shutdown are process
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
Incompatible settings fail before stopping the old process, with instructions
to restore those settings or explicitly rebuild from Agent settings > Harness.
The fingerprint is SHA-256 so secrets are not written to labels or logs.

Downloading a harness update only pins the selected image for a future environment.
Applying it to an existing environment requires a separate, confirmed rebuild.
Rebuild requires an idle Agent under the runtime lifecycle barrier, and warns
that installed tools and unmounted files will be lost. It removes the old
environment; the next turn creates one using the saved settings. No automatic
update is allowed to discard a retained writable layer.
After successful removal, clear the Agent's persisted task and conversation
native session IDs while still holding the runtime boundary. Their harness
storage may have lived in the deleted layer. Preserve logical history and start
fresh harness sessions on subsequent turns. A rejected rebuild leaves IDs intact.

Before explicit removal, inspect all containers (including stopped and unlabelled
containers) for namespace, volume-source, and link dependencies. If any reference
the environment, preserve it and report their names. Do not stop the environment
first and never recursively remove agent-created dependent containers. Engine
inspection failures fail closed. Check every owned candidate, including legacy
containers from an older installation layout, before deleting any environment.
The engine remains authoritative if another
trusted host-engine client changes dependencies during removal. Host-engine access
remains opt-in as specified in ADR-027.
Agent deletion must also stop if cleanup of an active legacy attempt container
fails, retaining the Agent's configuration and records for repair and retry.

Configured startup commands initialize a retained environment once. A marker is
written only after every command succeeds; a failed initialization is retried
on the next process start. Changing a startup command changes the container
specification and therefore requires a confirmed rebuild before it takes effect.

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
  mounts, credentials, environment, or commands require explicit user action; they
  never silently reuse stale authority or discard installed tools. Workspace and
  explicitly mounted host or named-volume data survive a confirmed rebuild.
- Project deletion is destructive for unmounted container state. The existing
  deletion confirmation is therefore the user-visible cleanup boundary.
- ACP sessions remain durable independently of the live process. Persistence
  and `session/resume`, `session/load`, or `session/fork` provide recovery after
  the unavoidable restart boundaries above.

## Validation

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
