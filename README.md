<p align="center">
  <img src="https://github.com/XpressAI/xpressclaw/blob/7a455d7bf77caf6dafdead4d37c79c7e3f6be809/docs/assets/xpressclaw-banner.jpeg" alt="xpressclaw" width="600">
</p>

<h3 align="center">Distributed, Multiplayer Control Plane for Native Agent Work</h3>

<p align="center">
Run Codex, Claude Code, DeepSeek Harness, OpenCode, and other native harnesses as isolated workers on an always-on machine. Coordinate people and Agents through durable Projects, Conversations, Tasks, memory, and multi-agent workflows from any connected client.
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

Codex, Claude Code, DeepSeek Harness, and OpenCode already supply excellent agent loops. xpressclaw is the **control plane around them**: durable work, automation, isolation, devices, and a UI for outcomes rather than terminals.

- **Multiplayer Project spaces** — Coordinate people and specialized Agents through shared Conversations, Tasks, workflows, files, and memory.
- **Distributed by design** — Keep Agents working on an always-on desktop or server while browsers and future native clients reconnect from other devices.
- **Harness-owned intelligence** — The selected product owns reasoning, tools, and subagents; xpressclaw is its ACP client, not another agent framework in front of it.
- **Structured interface** — See tasks, attempts, artifacts, questions, and review decisions without watching a terminal.
- **Native desktop app** — Tauri installers for macOS, Windows, and Linux with a system tray. Runs in the background, always available.
- **Automation-first** — Queue tasks, run recurring schedules, and express implementation/review loops as workflows.
- **Isolated and continuous** — Every Agent reuses one initialized Docker/Podman environment while Project Conversations stay available alongside task work.
- **Container-aware workspaces** — Opt trusted agents into a separate runner variant with Docker CLI, Compose, Buildx, and access to the host Docker/Podman engine.

## Features

### Real-time Control Center

Leave the Control center open to see working Agents, active Task and
Conversation turns, items needing input, canonical tool calls, context usage,
and attributed Git line changes across every Project. One durable live feed
replays missed responses and status changes after reconnecting, with direct
links back to the relevant work. See the [Control center guide](docs/control-center.md).

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
- **DeepSeek Harness:** runs through the maintained openma-ai ACP adapter and reuses `~/.dsh`
- **OpenCode:** uses its built-in ACP server
- **Custom:** any image and command that speaks ACP over stdin/stdout

Each Agent keeps an isolated, reusable environment for its selected harness,
workspace, installed tools, and caches. Trusted workspaces can optionally use
the host container engine for existing Compose-based workflows.

For DeepSeek Harness, sign in on the control-plane host with `dsh-acp login`
or use `dsh web` and save a model credential in its Settings page. XpressClaw
mounts the resulting `~/.dsh` directory only when host login is enabled; no API
key belongs in `xpressclaw.yaml`. See the [DeepSeek Harness guide](docs/deepseek-harness.md).

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
name the Agent, and select Codex, Claude Code, DeepSeek Harness, OpenCode, or a custom ACP
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
from a laptop or phone, choose the connection boundary that fits your network.

For example, from a laptop with SSH access to the control-plane host:

```bash
ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host
```

Then open `http://localhost:8935` on that laptop. An HTTPS reverse proxy is
another option. For a direct Tailscale or operator-trusted LAN connection,
choose `0.0.0.0` or `::` in **Settings → Instance** and explicitly confirm
unauthenticated access, or enable XpressClaw password/token authentication.
Authentication protects the app but does not supply TLS. Never expose the raw
port to the public internet.

Desktop can save local and remote instance profiles. Profile credentials stay
in the operating-system keychain, and the automatic local sidecar keeps
running while the app is connected to a remote instance.

Browser disconnection does not cancel work. Reopening the same instance loads
durable state and live streams reconnect; after a control-plane process
restart, interrupted work is recovered into the queue. See
[Remote access](docs/remote-access.md) for direct-tailnet, password/token,
SSH, HTTPS proxy, and Desktop-profile guidance.

## Common Workflows

- Use a **Project Conversation** to coordinate people and several Agents, or
  **New Work** to send private work directly to one Agent.
- Turn larger requests into durable **Tasks** with progress, artifacts,
  provenance, results, and errors that remain visible after reconnecting.
- Use **Automations → Schedules** for recurring or one-off work and
  **Automations → Workflows** for multi-Agent implementation/review or goal
  loops.
- Let the scoped GitHub integration publish a task's pull request, monitor
  review feedback, and resume the same Agent context until approval or merge.
- Optionally run local GitBucket and Jenkins collaboration services for
  selected Agents without changing existing GitHub flows.

See the guides for [workflows](docs/workflows.md),
[scheduling](docs/scheduling.md), [local collaboration](docs/local-collaboration.md),
[Project management and deletion](docs/projects.md),
[remote access](docs/remote-access.md), and the [Control center](docs/control-center.md).

## CLI Essentials

```
xpressclaw init [INSTANCE]   Optionally provision a control-plane instance
xpressclaw up [--detach]     Start the default local instance
xpressclaw down              Stop its detached control-plane process
xpressclaw status            Show Agent queue status
xpressclaw sync ...          Explicitly fetch/publish portable Project state
```

Most CLI/server users only need `xpressclaw up`, `status`, and `down`. `init`
is optional; it takes a positional instance directory, while `up` and `down`
select an alternate directory with `--instance`:

```bash
xpressclaw init /srv/xpressclaw/staging
xpressclaw up --detach --instance /srv/xpressclaw/staging --port 9001
xpressclaw down --instance /srv/xpressclaw/staging --port 9001
```

The instance directory is the directory containing `xpressclaw.yaml`; it is
not a Project repository or Agent workspace. See the complete
[CLI command reference](docs/commands.md) and
[configuration reference](docs/configuration.md) for alternate instances,
ports, compatibility flags, and YAML settings.

### Git-backed Project Synchronization

Use **Settings → Project sync** to fetch or publish a Project's portable state
through a separate Git repository. The CLI supports the same explicit flow:

```bash
cd /path/to/platform
xpressclaw sync init --project platform \
  --remote git@github.com:your-org/xpressclaw-data.git
xpressclaw sync publish
xpressclaw sync fetch
```

Synchronization never runs in the background, and credentials and local
runtime settings are not published. For project-name discovery, manifests,
the portable/private data boundary, and conflict handling, see the
[Git-backed Project synchronization guide](docs/project-sync.md).

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
