# Git-backed Project synchronization

XpressClaw can keep a Project's collaboration data in a dedicated Git
repository. The main project only preserves a small `.xpressclaw.yml` pointer;
it does not need to be a Git repository itself. Synchronization is always an
explicit CLI operation. Starting XpressClaw, editing source code, and updating
the main project never fetch or publish collaboration data.

## What is synchronized

The version 1 store carries portable Project data:

- Project metadata and Project Agents;
- tasks, dependencies, and task messages;
- Conversations, participants, and messages;
- reusable workflows associated with the Project; and
- Project memory notes, tags, and links, unless disabled by the manifest.

Conversation and task messages are written as separate append-oriented records.
Each record has a stable unique ID and an optional parent-record ID. Message
identity, content, authorship, timestamps, and parent relationships are
immutable; a Conversation message's linked-task relationship can change when
that task is linked or deleted. This avoids a single frequently rewritten
transcript and allows Git to merge independent message additions more cleanly.

Runtime state remains local: work attempts, queues, active turns, hidden idle
tasks, internal task context, workflow instances, schedules, logs, connector
state, binary attachments, generated embeddings, and retained runner sessions
are not synchronized. Existing local records that are absent from a fetched
snapshot are retained; version 1 fetches do not apply remote deletions.
Project-memory embeddings are rebuilt locally during fetch.

Agent configuration is split deliberately:

| Shared | Always local/private |
| --- | --- |
| backend, provider/model names, runner kind/image/model | API keys, bearer tokens, base URLs |
| tool and skill names | workspace paths, environment variables, volumes |
| non-secret ACP session options and commands | MCP selections/definitions/headers, hooks |
| budgets, rate limits, wake rules, idle prompt | subscription auth, SSH forwarding, container-engine access |

On fetch, shared fields are merged into `xpressclaw.yaml`; local fields for an
existing Agent are preserved. A newly imported Agent gets the main project
directory as its local workspace and no credentials.

Imported workflow definitions are disabled when they are new to an
installation, preventing a fetch from activating scheduled automation.
Existing local enable/disable state is preserved.

## Manifest

Run `sync init` once for an existing local Project:

```bash
xpressclaw sync init \
  --project-id 3a2a4b0e-example \
  --remote git@github.com:acme/xpressclaw-data.git \
  --branch main \
  --store-path projects/product-api \
  --no-project-memory \
  --project-dir /work/product-api \
  --workdir /work/xpressclaw-control
```

`--store-path` defaults to `projects/<project-id>`, `--branch` defaults to
`main`, and both directory options default to the current directory. The
command creates `/work/product-api/.xpressclaw.yml` without contacting the
remote:

```yaml
version: 1
project_id: 3a2a4b0e-example
store:
  remote: git@github.com:acme/xpressclaw-data.git
  branch: main
  path: projects/product-api
share:
  project_memory: false
```

Commit or otherwise preserve only this manifest with the main project. The
manifest schema is strict: unknown fields, unsupported versions, unsafe refs or
paths, and credential-bearing remote URLs are rejected. `path` must be a
portable relative path and cannot traverse `.git` or symlinks.

Use `--no-project-memory`, or set `share.project_memory: false`, when Project
memory is intentionally local. Fetch then ignores remote memory and publish
omits local memory. Because the manifest is shared, this is a Project-wide
policy.

The remote may use HTTPS, SSH/SCP, Git, or `file://` syntax. An absolute local
repository path is also accepted for local testing. Relative filesystem paths
and Git remote-helper transports are rejected because synchronization runs in
an isolated temporary checkout.

## Publish and fetch

Stop the control plane, or at least wait until the Project has no active task,
Conversation turn, or workflow, before synchronizing.

Publish the current portable snapshot:

```bash
xpressclaw sync publish \
  --project-dir /work/product-api \
  --workdir /work/xpressclaw-control
```

Once a Project workspace contains `.xpressclaw.yml`, the same explicit Fetch
and Publish operations are available from **Settings → Project sync** in the
web UI. The page discovers manifests from the host workspaces assigned to the
Project's Agents, shows the configured remote, branch, store path, and last
successful sync, and uses the control-plane process's existing Git
credentials. Nothing is synchronized in the background.

The first publish may create the configured branch and store path. If that
Project path already exists remotely, publish requires a prior fetch. Later
publishes require that Project path to match the snapshot last observed by
fetch or publish. Commits that only change other paths in the shared store do
not create false conflicts. The final push is never forced, so a concurrent
remote update fails safely; fetch before retrying.

Fetch on another installation:

```bash
xpressclaw sync fetch \
  --project-dir /work/product-api \
  --workdir /work/xpressclaw-control
```

Fetch validates every YAML record and reference before updating SQLite or
`xpressclaw.yaml`. A first fetch into an already populated local Project, or a
fetch where both local and remote portable state changed, stops with a conflict
error. If the configured remote Project snapshot has not changed, an ordinary
fetch is a no-op and preserves unpublished local edits. After reviewing a
conflict, `--force` acknowledges a **non-destructive merge**; it does not delete
local data. The Settings page only offers this merge after reporting the
conflict and applies imported Agent configuration to the running control
plane after a successful fetch. Restart XpressClaw after a CLI fetch so the
separate server process reloads imported Agent configuration. For records with
the same stable ID, fetched shared fields replace their local counterparts;
immutable message fields must match exactly, while a Conversation message's
linked-task relationship follows the fetched record. Local records that exist
only on the receiving installation remain present.

## Git credentials and secret safety

Git is required for the synchronization store. XpressClaw invokes Git in a
temporary checkout and uses each user's existing SSH agent or Git credential
helper. The remote repository must already exist; the first publish can create
the configured branch and Project path, but not the repository itself. Put no
username, password, token, signed query parameter, or private key in
`.xpressclaw.yml`. Credentials are never copied into the synchronized repository
or synchronization metadata.

Publishing also rejects high-confidence credential patterns, including bearer
tokens, common Git-host tokens, Slack tokens, and private-key blocks. This is a
safety net, not a general secret scanner: do not put secrets in task or
Conversation text intended for sharing.

## Store format and compatibility

The Git repository uses merge-friendly files under the configured path:

```text
projects/product-api/
├── .xpressclaw-store.yml
├── project.yml
├── agents/
├── tasks/
├── task-dependencies/
├── task-messages/
├── conversations/
├── conversation-participants/
├── conversation-messages/
├── workflows/
├── memory-notes/
└── memory-links/
```

Entity filenames are SHA-256 hashes of stable record keys, so IDs cannot
escape their directory and Unicode IDs remain portable. Record files are
limited to 2 MiB and each entity directory to 100,000 records. Symlinks and
unknown root entries or non-YAML files inside entity directories are rejected.

Database migration 36 adds synchronization mappings and baselines
automatically. Existing `xpressclaw.yaml` files and projects continue to work
without a manifest and are never synchronized implicitly. Version 1 manifests
and stores intentionally fail closed on newer schema versions instead of
guessing at a migration.
