# ADR-046: Xpress AI Connect

Status: Implementation proposal for review

## Decision

Connect an XpressClaw installation to an Xpress AI account, grant it access to
selected platform projects, and publish selected local Agents in those projects.
Platform chats and tasks keep their existing ownership and scheduling. Their
selected Agent executes through its existing XpressClaw container and harness.
Provider credentials and the workspace remain on that installation.

This implements the requested Connect flow across XpressClaw and the platform.
The companion platform ADR is `adr/088-xpress-ai-connect.md`. Both changes must be
released together; protocol version 1 is required. The reviewed bases are
XpressClaw `04fcef778a59223a73f4a252c2cca5aad8d6aa8f` and platform
`dd93f44be02228b29bdbb95c9097c1140e66cb2b`.

## Account and project consent

The owner starts pairing in Settings → Xpress AI Connect. A ten-minute pairing
session has a private device proof and a separate verification code. The owner
opens the platform verification page, signs in, verifies the instance, and
explicitly selects allowed projects. Approval requires authenticated browser
access and same-origin mutation headers. Opening the link alone grants nothing.
The owner returns to XpressClaw and confirms approval to collect the credential.

The platform stores only a credential hash. XpressClaw stores its credential and
pending device proof in `xpress-ai-connect.json` in its instance data directory,
with owner-only file permissions. Neither secret is returned in settings views
or distributed to harnesses. This file is separate from Git project sync.
Credential collection is one-time: if its response is lost, revoke the unused
instance in the platform settings and pair again.

Bindings name a local Project/Agent and an authorized canonical platform project
and Agent identity. A local Project maps to one platform project; a local Agent
has one identity per pairing. Names cannot change a saved binding's identity.
Disconnecting and republishing the same binding advances its generation and
invalidates its old execution leases. Moving a binding to a different project
requires a new pairing and identity. Existing cloud Agent names are not replaced.

Each installation must pair separately. Copying a live instance credential and
running multiple control planes with that identity is unsupported. This release
does not implement active-active connector ownership or credential rotation.

## Transport and execution

All network connections originate at XpressClaw. HTTPS polling uses
`/api/connect/v1`, a dedicated opaque-credential header, no redirects, bounded
responses, and request deadlines. HTTP is accepted only for loopback development.
No public local listener or external RabbitMQ access is needed.

The platform persists each command before advertising it. Chat commands use the
source message ID for replay detection. Task submissions retain upstream keys
when available; ordinary starts save their pending response and command in the
same database transaction. Accepted work is never automatically re-dispatched
because its connection disappeared.

Before executing, the connector obtains a 90-second renewable lease, then
atomically verifies its locally approved binding, records the command digest,
and creates a targeted Conversation turn in SQLite. A crash between lease
acceptance and local admission can redeliver the same command. Repeated admitted
commands return the recorded execution, including after completion. Changed
payloads under an existing command ID are rejected.

Each command gets its own local execution Conversation. The platform supplies
bounded recent context; the container workspace and project memory persist.
Full harness session continuity between platform turns is outside this version.
Imported context does not invoke local conversation fan-out. The local transcript
is a projection: users continue the conversation in Xpress AI. Existing unlinked
local work retains its normal behavior.

Receipts and final results remain in the local journal until acknowledged.
The platform commits a final chat message transactionally with the command's
terminal status, so lost acknowledgement does not publish it again. This provides
replay protection for protocol effects, not exactly-once arbitrary shell/network
side effects performed by a model.

## Platform integration

Published Agents use ordinary Agent records with `executionPlacement=xpressclaw`.
Cloud-only model, URL and provider-token columns are nullable for these records;
cloud records retain their configuration constraint. Connected Agents appear in
project catalogs, chat participant selectors, and task assignment lists. Their
runtime/model is managed in XpressClaw, so cloud start/restart/configuration and
OpenAI endpoint controls are not offered for these rows.

`ConnectAgentClient` routes asynchronous task operations to the command outbox,
preserving the normal result processor. `AgentTurnWorker` persists selected chat
turns to the same outbox. Transient submission failures requeue; denied access
cannot fall back to a cloud Agent. Group-chat selection remains platform-owned.
Connected Agents have no cloud internal-reasoner endpoint; the existing
unavailable-reasoner backstop handles their non-mentioned group-chat selection.
Voice, cloud desktop controls, and cloud integration provisioning are not exposed
through Connect.

## Scoped tools and files

The bundled XpressClaw MCP checks the execution's local Conversation and Agent,
then proxies supported calls with the platform command ID. The platform checks
instance ownership, binding generation, current project membership, assignment,
chat participation where needed, and a live uncancelled lease on every callback.
The harness never receives the instance credential or chooses a platform project.

Supported tools are interim messages, follow-up task creation assigned to the
same Agent, assigned-task inspection, checklist updates, requested task status,
attachment listing/download, and file publication. Task status is staged until
the final response is accepted. Done still requires the ordinary checklist and
source-chat handoff checks. The normal task result processor handles subsequent
completion, waiting, retries, and queued user messages. Local wake-ups, local memory-editing tools, and local
delegation are hidden and rejected in connected turns. Connected callbacks use
a purpose-bound per-Agent capability rather than the local root token.

Publish files explicitly through the connected message/file tools before the
final reply. Files are limited to 8 MiB each and 20 MiB per command, stored as
platform-owned artifacts, and returned as authenticated download links. Input
chat uploads and connected artifacts can be downloaded only within the authorized
source conversation; returned bytes include a SHA-256 digest. Arbitrary URLs and
container paths are not platform download links. Identical message/file and
follow-up-task requests within one command are deduplicated by request digest.

Permission prompts and harness elicitation remain in the local XpressClaw UI.
They are not yet interactive platform chat cards. A custom replacement for the
bundled XpressClaw MCP is not supported for connected Agents.

## Cancellation and recovery

The platform cancels queued work without disclosing its payload after access is
removed. Running work receives cancellation through receipt heartbeats. The
local watchdog interrupts expired leases independently of model requests. Lease
renewal cannot reactivate a locally disabled binding or an older generation.
Disconnect disables local authority first, then attempts remote revocation. The
UI explains when that remote request could not complete; platform settings also
allow revocation without the instance being online.

Revocation cannot undo external effects or instantly stop an offline process.
An expired platform lease becomes an unknown-execution failure; local restart
also reports `execution_state_lost_after_restart` instead of rerunning admitted
work. The existing platform task processor pauses these outcomes for recovery.
Cancellation requests use existing local turn/process interruption machinery.

## Validation and release

Tests cover command replay/conflicts, local binding and Agent authorization,
generation changes, cancellation, lease expiry/renewal, restart recovery, one-time
pairing proofs, explicit project grants, task submission, stale callbacks,
checklist completion, file boundaries, protocol HTTP delivery, and browser
pairing/publishing. PostgreSQL integration executes the actual migration and
Hibernate queries, including file persistence and result replay.

Release requires both repositories' changes, migrations, and a rebuilt XpressClaw
binary (the bundled MCP source travels with the control plane). A deployment
smoke test should pair a real installation, publish an Agent, exchange a chat
message, complete a task with a file, and revoke it during execution. No production
account pairing, model invocation, or deployment is performed by the code tests.
