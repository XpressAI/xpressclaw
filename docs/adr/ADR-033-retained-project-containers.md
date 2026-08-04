# ADR-033: Retained Project Containers

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

ACP processes should remain bounded to a turn, but process lifetime and
development-environment lifetime do not need to be the same.

## Decision

Each configured project agent owns one retained container named
`xpressclaw-<agent-id>`. It is allocated lazily by the first task. XpressClaw
starts the ACP command and attaches it for a turn, then stops the container
without removing it. The next turn restarts the same container, preserving its
writable layer, including `/home/node` and `/tmp`, while still establishing a
fresh ACP transport and resuming or forking the durable ACP session normally.

Project queue dispatch remains serialized. A running queue row is also the
lease on the shared container, including the short cancellation interval after
an attempt becomes terminal but before its process has stopped. This prevents
a new turn from restarting the environment immediately before an older
cancellation path stops it.

Retained containers carry lifecycle and runner-specification labels. The
specification fingerprint covers the image reference, command, environment,
mounts, limits, working directory, and network configuration. XpressClaw
recreates the container when that fingerprint changes or its image no longer
matches the current local image. The fingerprint is SHA-256 so secrets in the
environment are not written directly to container labels or logs.

Configured startup commands initialize a retained environment once. A marker
is written only after every command succeeds; a failed initialization is
retried on the next start. Changing a startup command changes the container
specification and therefore creates a clean environment.

Control-plane shutdown and startup stop configured project containers but
retain them. Deleting a project agent removes its container and writable
layer. Containers belonging to deleted configuration and old attempt-scoped
or pre-ADR-025 layouts are removed during startup cleanup.

## Consequences

- Follow-up turns see tools, caches, and files installed outside the mounted
  workspace instead of rebuilding them.
- Idle projects consume disk but no CPU or memory. Docker/Podman administrators
  can still inspect the stopped containers directly.
- Updating runner mounts, credentials, environment, image, or command creates
  a clean container. Workspace and explicitly mounted host or named-volume
  data remain governed by those mounts.
- Project deletion is destructive for unmounted container state. The existing
  deletion confirmation is therefore the user-visible cleanup boundary.
- ACP sessions remain durable independently of the container. A retained
  container improves environment continuity; it does not replace ACP
  `session/resume`, `session/load`, or `session/fork`.
