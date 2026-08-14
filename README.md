<p align="center">
  <img src="https://github.com/XpressAI/xpressclaw/blob/7a455d7bf77caf6dafdead4d37c79c7e3f6be809/docs/assets/xpressclaw-banner.jpeg" alt="xpressclaw" width="600">
</p>

<h3 align="center">Distributed, Multiplayer Control Plane for Native Agent Work</h3>

<p align="center">
Run Codex, Claude Code, OpenCode, and other native harnesses as isolated workers on an always-on machine. Coordinate people and Agents through durable Projects, Conversations, Tasks, memory, and multi-agent workflows from any connected client.
</p>

<p align="center">
<a href="https://xpressclaw.ai">Website</a> &bull;
<a href="https://hub.xpressclaw.ai">Hub</a> &bull;
<a href="https://github.com/XpressAI/xpressclaw/blob/main/docs/development.md">Develop</a> &bull;
<a href="https://discord.com/invite/vgEg2ZtxCw">Discord</a>
</p>

<p align="center">
<a href="https://github.com/XpressAI/xpressclaw/blob/main/LICENSE"><img src="https://img.shields.io/github/license/XpressAI/xpressclaw?color=brightgreen" alt="License"></a>
<a href="https://github.com/XpressAI/xpressclaw/releases"><img src="https://img.shields.io/github/v/release/XpressAI/xpressclaw?color=yellow" alt="Release"></a>
<img src="https://img.shields.io/badge/rust-stable-orange" alt="Rust">
</p>

---

Download the macOS, Windows, or Linux Desktop installer from
[Releases](https://github.com/XpressAI/xpressclaw/releases), launch it, and add
the repository you want an Agent to work in. Desktop creates and starts your
default local XpressClaw instance automatically—there is no `init` step.

The matching runner image is pulled automatically.

## Why xpressclaw?

Codex, Claude Code, and OpenCode already supply excellent agent loops. xpressclaw is the **control plane around them**: durable work, automation, isolation, devices, and a UI for outcomes rather than terminals.

- **Multiplayer Project spaces** — Coordinate people and specialized Agents through shared Conversations, Tasks, workflows, files, and memory.
- **Distributed by design** — Keep Agents working on an always-on desktop or server while browsers and future native clients reconnect from other devices.
- **Harness-owned intelligence** — The selected product owns reasoning, tools, and subagents; xpressclaw is its ACP client, not another agent framework in front of it.
- **Structured interface** — See tasks, attempts, artifacts, questions, and review decisions without watching a terminal.
- **Native desktop app** — Tauri installers for macOS, Windows, and Linux with a system tray. Runs in the background, always available.
- **Automation-first** — Queue tasks, run recurring schedules, and express implementation/review loops as workflows.
- **Isolated and continuous** — Every Agent reuses one initialized Docker/Podman environment while Project Conversations stay available alongside task work.
- **Container-aware workspaces** — Opt trusted agents into a separate runner variant with Docker CLI, Compose, Buildx, and access to the host Docker/Podman engine.

## Features

### Project Conversations

Projects are the top-level collaboration boundary. Each Project can contain
several specialized Agents, and each Agent can participate in several shared
Conversations. Mention one Agent or ask all participants; Conversation turns
use independent ACP sessions, so an Agent can answer while its serialized task
lane is busy.

Turn a Conversation message into a linked Task with **Continue with task**, or
start a reusable workflow with typed inputs. Agents can create linked Tasks for
themselves, publish task results back to the Conversation, and share files that
people or other Agents can download into their workspaces.

### Persistent Agents

Each Agent is a durable execution identity inside a Project. It has a name,
workspace, retained container, and replaceable ACP harness. Task timelines
record every attempt and where it came from, while Project memory and
Conversations provide shared context. XpressClaw does not invent a persona or
system prompt.

### Autonomous Task Execution

ACP harnesses pick up tasks from a queue and publish standard progress, plans,
tool activity, and results. Schedule recurring work with cron expressions.
Workflows coordinate different Agents—for example, one using Codex to
implement and another using Claude to review in a loop—and can report every
step back to the Conversation that started the run.

### Multiple ACP Harnesses

- **Codex:** reuses an eligible host ChatGPT/Codex login
- **Claude Code:** reuses an eligible host Claude subscription login
- **OpenCode:** uses its built-in ACP server
- **Custom:** any image and command that speaks ACP over stdin/stdout

Codex and Claude use ACP Registry adapters. Each built-in runner image contains
one harness product and its ACP server. Its Agent-owned writable layer keeps
language SDKs, tools, and caches installed during earlier turns without baking
them into the runner image. Agents whose workspaces need existing Compose-based
development and test workflows can explicitly enable trusted host-engine access.

### Privacy & Safety

- **Project boundary, Agent isolation** — shared memory and Conversations stay inside one Project; every Agent keeps its own retained container and task ACP process
- **Explicit resources** — workers receive the configured workspace and volume mounts
- **Visible provenance** — every queued request records whether it came from a person, schedule, task, or workflow
- **Local control data** — timelines and task state remain local; agent traffic follows the selected provider's terms
- **Credential boundary** — subscription auth is mounted only into the configured Agent container built from an image you trust

### Full Observability

Session events, attempt lifecycle, artifacts, provenance, and cancellation. Know what ran at 3am and why.

## The User Model

- An **instance** is one long-running XpressClaw control plane plus its local
  configuration and data. It can own many Projects and repositories. Desktop
  manages one default local instance at `~/.xpressclaw`.
- A **client** is the Desktop window or a browser connected to an instance.
  Closing a client does not stop the instance, its queues, or its Agents.
- A **Project** is the collaboration and memory boundary containing Agents,
  Conversations, Tasks, and workflows.
- An **Agent** is an execution identity with one ACP harness, retained
  environment, and workspace. Its workspace may be an existing repository or
  an XpressClaw-managed folder.

`xpressclaw.yaml` configures an instance; it does not add a repository and
does not belong in every repository. Repository-local harness instructions
such as `AGENTS.md`, `CLAUDE.md`, or product-specific equivalents stay with the
repository and are read by the selected harness.

## Quick Start

### 1. Install XpressClaw

#### Desktop (recommended)

The official [Releases](https://github.com/XpressAI/xpressclaw/releases) page
publishes these Desktop installers:

- macOS: signed and notarized `.dmg` files for Apple Silicon and Intel;
- Windows: `.exe` and `.msi` installers for x64; and
- Linux: `.deb` and `.rpm` packages for x64.

Install and launch the app. It starts the bundled control plane from
`~/.xpressclaw`, opens first-run setup, and stays available in the system tray.
The Desktop installer does not install a separate `xpressclaw` command on
`PATH`.

#### CLI/server (macOS and Linux)

Beginning with the first stable `0.3.0` release, install the standalone
CLI/server with:

```bash
curl -fsSL https://raw.githubusercontent.com/XpressAI/xpressclaw/main/install.sh | sh
```

The script discovers GitHub's latest stable release at runtime, deliberately
ignores prereleases, verifies the published SHA-256 checksum, and installs the
single `xpressclaw` binary to `~/.local/bin`. It supports Apple Silicon and
Intel macOS plus x64 Linux. Add that directory to `PATH` if needed, then run:

```bash
xpressclaw up
```

This discovers or creates the default instance at `~/.xpressclaw` and opens
setup at `http://localhost:8935`. Windows users should install Desktop; stable
Releases also attach a standalone x64 `.zip` for manual CLI/server installs.

### 2. Create your first Project and Agent

Choose an existing repository folder or start with an empty managed workspace,
name the Agent, and select Codex, Claude Code, OpenCode, or a custom ACP
harness. XpressClaw creates the first Project around that Agent. Add more
Projects and Agents from the UI; you do not run `init` for each repository.

### 3. Start work

Open a Project Conversation for shared coordination, use **Continue with
task** for durable work, or send private work directly to one Agent from
**New Work**. Closing the window does not stop the local control plane.

### Requirements

- Docker or Podman (required for worker isolation)
- At least one supported harness login on the host
- A built-in ACP runner image, or your own ACP-compatible image

## Remote Access

The control plane is the durable machine running Agents; Desktop and the web
UI are clients. To leave work running on a desktop or server and reconnect
from a laptop or phone, keep XpressClaw bound to its default loopback address
and put an authenticated transport in front of it.

For example, from a laptop with SSH access to the control-plane host:

```bash
ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host
```

Then open `http://localhost:8935` on that laptop. An authenticated HTTPS
reverse proxy is another option. XpressClaw does not yet provide native remote
authentication, so it refuses non-loopback binds unless
`--allow-insecure-remote` explicitly acknowledges that another security layer
protects the address. Never expose the port directly to a LAN or the internet.

Browser disconnection does not cancel work. Reopening the same instance loads
durable state and live streams reconnect; after a control-plane process
restart, interrupted work is recovered into the queue. See
[Remote access](docs/remote-access.md) for the current security boundary and
Desktop limitations.

## What Can It Do?

**Talk to a Project or send durable work:**

Open a Project Conversation to coordinate with one or more Agents, attach
files, and keep talking while task work runs. Use **Continue with task** when a
request needs a durable work attempt, or send private work directly to one
Agent from **New Work**.

**Schedule recurring tasks or one-off follow-ups:**

Create a task in **Tasks**, select its agent, then add a cron schedule
in **Automations → Schedules**. Scheduled work enters the same queue and timeline as a
message sent by a person. Native harnesses can arm a durable `schedule_wakeup`
when a long-running external job needs to be checked hours later; the future
turn resumes the same agent conversation.

**Review what happened while you were away:**

Open the Agent task timeline for attempt status, results, artifacts, provenance,
and errors. `xpressclaw status` remains available as a small health check for
the local control plane.

**Coordinate multiple products:**

Use **Automations → Workflows** for implementation/review loops, goal loops,
and other work that moves between Agents. Run them from a Conversation to keep
their Tasks and results in that shared context, run them independently with
typed inputs, or attach a recurring cron trigger. See
[Workflows](docs/workflows.md) for the definition format and execution
behavior. The CLI deliberately does not duplicate these product surfaces.

**Finish pull requests through review:**

For ordinary tasks, the scoped GitHub tool publishes completed work ready for
review and keeps the task active. XpressClaw durably checks for review comments,
resumes the same conversation to address them, and releases the next queued
task only after approval or merge. Explicit workflow steps may still use draft
PRs and their own wait logic.

## Configuration

The default instance is `~/.xpressclaw`; `xpressclaw up` creates its directory
and opens first-run setup when needed. Setup writes `xpressclaw.yaml`, and
Agents are added in the web UI. A new explicitly initialized instance keeps
its database, managed workspaces, logs, and PID file under its own directory:

```yaml
system:
  isolation: docker
  data_dir: /home/me/.xpressclaw
  workspace_dir: /home/me/.xpressclaw/workspaces

agents: []

```

## Development

Source builds, repository architecture, runner-image development, release
mechanics, and test commands live in the
[Developer Guide](docs/development.md).

## CLI Reference

```
xpressclaw init [INSTANCE]   Optionally provision a control-plane instance
xpressclaw up [--detach]     Start the default local instance
xpressclaw down              Stop its detached control-plane process
xpressclaw status            Show Agent queue status
xpressclaw sync ...          Explicitly fetch/publish portable Project state
```

`init` takes an optional positional directory, while `up` and `down` select
that directory with `--instance`. For the default instance:

```bash
xpressclaw init
xpressclaw up
```

For an alternate instance, use the same directory when starting and stopping
it. Give concurrently running instances different ports:

```bash
xpressclaw init /srv/xpressclaw/staging
xpressclaw up --detach --instance /srv/xpressclaw/staging --port 9001
xpressclaw down --instance /srv/xpressclaw/staging --port 9001
```

The instance directory is the directory containing `xpressclaw.yaml`; it is
not a Project repository or Agent workspace. `xpressclaw init .` followed by
`xpressclaw up --instance .` preserves the former current-directory flow.
`--workdir <DIR>` remains a deprecated alias for existing scripts, and an
existing current-directory `xpressclaw.yaml` remains discoverable when no
default instance has been configured. See [CLI commands](docs/commands.md).

### Git-backed Project Synchronization

XpressClaw can share a Project's portable collaboration state through a
separate Git repository. Agents, Tasks, Conversations, workflows, and
optionally Project memory are included. Runtime state, workspace paths,
credentials, tokens, environment variables, and other user-specific settings
remain local.

Synchronization is always explicit: ordinary XpressClaw use and changes to the
main project never fetch or publish this data. The main project does not need
to be a Git repository; it only needs to preserve the generated
`.xpressclaw.yml` manifest.

First, create the remote synchronization repository and configure Git access
with your own SSH agent or credential helper. Then initialize synchronization
for an existing local Project from the repository being synchronized:

```bash
cd /path/to/platform
xpressclaw sync init \
  --project platform \
  --remote git@github.com:your-org/xpressclaw-data.git
```

`--project` accepts a visible name or exact canonical ID. Ambiguous names are
reported with their IDs, and the Project page exposes a copyable canonical ID;
the old `--project-id <ID>` spelling remains supported. XpressClaw discovers
`xpressclaw.yaml` in the current/parent directories or in a single sibling
control-plane repository, then falls back to Desktop's
`~/.xpressclaw/xpressclaw.yaml`. Otherwise pass
`--control-plane-dir /path/to/xpressclaw-control`. This is distinct from
`--project-dir`, which means the repository receiving `.xpressclaw.yml`;
`~/.xpressclaw` is both Desktop's control-plane directory and the default local
data directory. CLI installations may keep `xpressclaw.yaml` elsewhere.
`--workdir` remains a backward-compatible alias for `--control-plane-dir`.

`--branch` defaults to `main`, and `--store-path` defaults to
`projects/<canonical-project-id>`. Use `--no-project-memory` if memory should
stay local. Initialization only creates `.xpressclaw.yml`; it does not contact
the remote or synchronize any data. Preserve that manifest with the main
project, but never put credentials in it.

Publish the first shared snapshot:

```bash
xpressclaw sync publish
```

Anyone with the same manifest can explicitly fetch the snapshot into their
local XpressClaw installation, work normally, and publish their updates:

```bash
# Before starting work
xpressclaw sync fetch

# After making XpressClaw Project changes
xpressclaw sync publish
```

Stop the control plane, or wait until the Project has no active work, before a
fetch or publish. Restart it after a fetch so imported Agent configuration is
reloaded. A first fetch into an already populated Project, or a two-sided
change, fails safely; review the conflict before retrying fetch with `--force`.
The forced fetch is a non-destructive merge and does not delete local-only
records.

Each user supplies their own Git and Agent credentials through local secure
configuration. Those values are never copied into `.xpressclaw.yml` or the
synchronized repository. For the full manifest schema, shared/private data
boundary, conflict behavior, and merge-friendly record format, see the
[Git-backed Project synchronization guide](docs/project-sync.md).

Default port: `8935` (override with `--port`).

Projects, Conversations, messages, tasks, schedules, workflows, results, and
configuration live in the web UI rather than a second CLI interface.

## From Open Source to Enterprise

xpressclaw is the open-source foundation. When your team needs collaboration, visual workflows, compliance, and enterprise support — [Xpress AI](https://xpress.ai) has you covered.

| | xpressclaw (Free) | Xpress AI (Enterprise) |
|---|---|---|
| Autonomous AI agents | :white_check_mark: | :white_check_mark: |
| Chat-first web UI | :white_check_mark: | :white_check_mark: |
| SOPs & scheduling | :white_check_mark: | :white_check_mark: |
| Local model support | :white_check_mark: | :white_check_mark: |
| Budget controls | :white_check_mark: | :white_check_mark: |
| Team collaboration | | :white_check_mark: |
| Visual workflow builder (Xircuits) | | :white_check_mark: |
| iOS & Android apps | | :white_check_mark: |
| On-premise deployment | | :white_check_mark: |
| Role-based access control | | :white_check_mark: |
| Audit logging & compliance | | :white_check_mark: |
| Dedicated support & SLA | | :white_check_mark: |

[Request an Enterprise Demo](https://xpress.ai)

## Contributing

We welcome contributions. Source-build instructions, architecture, runner
development, tests, formatting, and release mechanics are in the
[Developer Guide](docs/development.md).

## Community

- **Website:** [xpressclaw.ai](https://xpressclaw.ai)
- **Hub:** [hub.xpressclaw.ai](https://hub.xpressclaw.ai)
- **Discord:** [discord.com/invite/vgEg2ZtxCw](https://discord.com/invite/vgEg2ZtxCw)
- **Twitter/X:** [@xpressclaw](https://twitter.com/xpressclaw)
- **Enterprise:** [xpress.ai](https://xpress.ai)

## License

[GPL-3.0](LICENSE)

---

<p align="center">
Built by <a href="https://xpress.ai">Xpress AI</a> — the team behind enterprise agent platforms for regulated industries.
</p>
