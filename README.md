<p align="center">
  <img src="https://github.com/XpressAI/xpressclaw/blob/7a455d7bf77caf6dafdead4d37c79c7e3f6be809/docs/assets/xpressclaw-banner.jpeg" alt="xpressclaw" width="600">
</p>

<h3 align="center">Control Plane for Native Agent Work</h3>

<p align="center">
Run Codex, Claude Code, OpenCode, and other native agents as isolated workers. Queue tasks, schedule recurring work, coordinate multi-agent workflows, and follow everything through one structured session UI.
</p>

<p align="center">
<a href="https://xpressclaw.ai">Website</a> &bull;
<a href="https://hub.xpressclaw.ai">Hub</a> &bull;
<a href="https://github.com/XpressAI/xpressclaw/blob/main/CONTRIBUTING.md">Contribute</a> &bull;
<a href="https://discord.com/invite/vgEg2ZtxCw">Discord</a>
</p>

<p align="center">
<a href="https://github.com/XpressAI/xpressclaw/blob/main/LICENSE"><img src="https://img.shields.io/github/license/XpressAI/xpressclaw?color=brightgreen" alt="License"></a>
<a href="https://github.com/XpressAI/xpressclaw/releases"><img src="https://img.shields.io/github/v/release/XpressAI/xpressclaw?color=yellow" alt="Release"></a>
<img src="https://img.shields.io/badge/rust-stable-orange" alt="Rust">
</p>

---

```bash
xpressclaw init
xpressclaw up
```

Open `http://localhost:8935`, create a project, choose its workspace,
and send it work. The matching runner image is pulled automatically. To build
the Codex runner locally instead:

```bash
docker buildx build --load -f harnesses/native/codex/Dockerfile -t xpressclaw-runner-codex:latest -t localhost/xpressclaw-runner-codex:latest harnesses/native
# Or: podman build -f harnesses/native/codex/Dockerfile -t localhost/xpressclaw-runner-codex:latest harnesses/native
```

## Why xpressclaw?

Codex, Claude Code, and OpenCode already supply excellent agent loops. xpressclaw is the **control plane around them**: durable work, automation, isolation, devices, and a UI for outcomes rather than terminals.

- **One logical session** — People, schedules, workflows, and ACP agents contribute to one event history.
- **Agent-owned intelligence** — The selected product owns reasoning, tools, and subagents; xpressclaw is its ACP client, not another agent framework in front of it.
- **Structured interface** — See tasks, attempts, artifacts, questions, and review decisions without watching a terminal.
- **Native desktop app** — Tauri installers for macOS, Windows, and Linux with a system tray. Runs in the background, always available.
- **Automation-first** — Queue tasks, run recurring schedules, and express implementation/review loops as workflows.
- **Isolated by default** — Every agent turn runs in a short-lived Docker/Podman container.
- **Container-aware projects** — Opt trusted projects into a separate runner variant with Docker CLI, Compose, Buildx, and access to the host Docker/Podman engine.

## Features

### Persistent Sessions

The primary interface is a durable event timeline labeled by project context. It accepts a new message while work is running and records where every event came from. Xpressclaw does not invent an agent identity or persona.

### Autonomous Task Execution

ACP agents pick up tasks from a queue and publish standard progress, plans, tool activity, and results. Schedule recurring work with cron expressions. Workflows coordinate different products—for example, Codex implementing and Claude reviewing in a loop.

### Multiple ACP Agents

- **Codex:** reuses an eligible host ChatGPT/Codex login
- **Claude Code:** reuses an eligible host Claude subscription login
- **OpenCode:** uses its built-in ACP server
- **Custom:** any image and command that speaks ACP over stdin/stdout

Codex and Claude use ACP Registry adapters. Each built-in runner image contains
one agent product and its ACP server. Language SDKs and
project services belong in a separate development environment rather than in
the runner image. Projects that need existing Compose-based development and
test workflows can explicitly enable trusted host-engine access.

### Privacy & Safety

- **Container isolation** — each work attempt runs in its own short-lived container
- **Explicit resources** — workers receive the configured workspace and volume mounts
- **Visible provenance** — every queued request records whether it came from a person, schedule, task, or workflow
- **Local control data** — timelines and task state remain local; agent traffic follows the selected provider's terms
- **Credential boundary** — subscription auth is mounted only into short-lived workers built from an image you trust

### Full Observability

Session events, attempt lifecycle, artifacts, provenance, and cancellation. Know what ran at 3am and why.

## Quick Start

### Option 1: Download Binary

Grab the latest release from [GitHub Releases](https://github.com/XpressAI/xpressclaw/releases).

```bash
xpressclaw init
xpressclaw up
# Open http://localhost:8935
```

### Option 2: Native App

Download the macOS, Windows, or Linux installer from [Releases](https://github.com/XpressAI/xpressclaw/releases). The app runs in the system tray and keeps the local control plane available in the background.

### Option 3: Build from Source

See [Building](#building) below.

### Requirements

- Docker or Podman (required for worker isolation)
- At least one supported agent login on the host
- A built-in ACP runner image, or your own ACP-compatible image

## What Can It Do?

**Send work from the session UI:**

Open a persistent session and describe the outcome. The request becomes a task and native work attempt while the UI remains available.

**Schedule recurring tasks or one-off follow-ups:**

Create a task in **Work → Tasks**, select its session, then add a cron schedule
in **Work → Schedules**. Scheduled work enters the same queue and timeline as a
message sent by a person. Native agents can arm a durable `schedule_wakeup`
when a long-running external job needs to be checked hours later; the future
turn resumes the same project conversation.

**Review what happened while you were away:**

Open the session timeline for attempt status, results, artifacts, provenance,
and errors. `xpressclaw status` remains available as a small health check for
the local control plane.

**Coordinate multiple products:**

Use **Workflows** for implementation/review loops and other work that moves
between sessions. The CLI deliberately does not duplicate these product
surfaces.

## Configuration

`xpressclaw init` creates a minimal `xpressclaw.yaml`. Sessions are added in the
web UI, which records the selected product image and workspace:

```yaml
system:
  isolation: docker

agents: []

```

## Building

### Prerequisites

- [Rust](https://rustup.rs/) stable toolchain
- [Node.js](https://nodejs.org/) 20+
- The [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/) for your platform when building the desktop app
- Docker or Podman (for isolated ACP agents)

### Build Everything

```bash
git clone https://github.com/XpressAI/xpressclaw.git
cd xpressclaw

# Build CLI, server, desktop app, and native runner images
./build.sh

# Skip runner images when you only need the application
./build.sh --skip-docker
```

### Build Individual Targets

```bash
# CLI only (also embeds the frontend)
cargo build --release -p xpressclaw-cli

# Core library
cargo build -p xpressclaw-core

# Server
cargo build -p xpressclaw-server

# The release CLI binary is target/release/xpressclaw
```

### Build the Desktop App (Tauri)

```bash
# Build everything including the Tauri desktop app
./build.sh

# For signed/notarized macOS builds
./build-signed.sh
```

### Release Versioning

The product version comes from `[workspace.package]` in `Cargo.toml`; run
`node scripts/release-metadata.mjs --check` to verify the Tauri, frontend,
lockfile, and bundled MCP metadata agree with it.

Each successful release receives the next numeric build and a tag such as
`v0.2.0-build.51`. The release workflow derives that number from existing
release tags, embeds it in `/api/health` and the About screen, and uses it as
the macOS bundle version. The Git commit remains available separately for
diagnostics.

### Build Native Worker Images

Build only the product you use. Each image is deliberately independent so it
can be versioned or extended without pulling in the other agent CLIs:

```bash
docker buildx build --load -f harnesses/native/codex/Dockerfile -t xpressclaw-runner-codex:latest -t localhost/xpressclaw-runner-codex:latest harnesses/native
docker buildx build --load -f harnesses/native/claude/Dockerfile -t xpressclaw-runner-claude:latest -t localhost/xpressclaw-runner-claude:latest harnesses/native
docker buildx build --load -f harnesses/native/opencode/Dockerfile -t xpressclaw-runner-opencode:latest -t localhost/xpressclaw-runner-opencode:latest harnesses/native
```

To build every standard and host-engine variant with whichever local runtime
is available, run `scripts/build-runner-images.sh`. Set
`CONTAINER_RUNTIME=docker` or `CONTAINER_RUNTIME=podman` to override automatic
detection.

The `Build & Push Runner Images` workflow publishes all six multi-architecture
images whenever their sources change on `main`, then verifies that they can be
pulled without credentials. GHCR creates each package as private on its first
publication, so an XpressAI organization owner must change each new package to
**Public** once in the package settings. Releases stop before publication if
any runner is unavailable anonymously.

The default images stay minimal. Add `--target runner-host` and use the
corresponding `xpressclaw-runner-<product>-docker:latest` tag to build an
opt-in image containing Docker CLI, Compose, and Buildx. Enabling host-engine
access mounts the control plane's Docker or rootless Podman socket; this gives
the runner control over that engine and is intended only for trusted agents.

Use `podman build` with the `localhost/` tags when Podman is your runtime. The ACP
compatibility label on current images prevents an older pre-ACP local tag from
being selected silently.

See `harnesses/native/README.md` for build commands, customization guidance,
and the separation between agent runners and development environments.

### Native harness capabilities

Each project session keeps the selected native product's own configuration.
When host login is enabled, XpressClaw mounts the complete Codex, Claude Code,
or OpenCode configuration directory—not just its token—so native skills,
plugins, hooks, custom agents, and user settings remain available. Project
configuration in the mounted workspace is discovered normally. Extra config
trees can be attached with per-session volume mounts and environment values.

MCP servers are defined once and enabled per harness. Stdio commands must be
absolute paths inside that harness image; remote HTTP and SSE endpoints do not
need to be installed in the image. ACP supplies the selected servers when a
session is created, resumed, or loaded.

After the first turn, the task composer and workflow editor show the commands,
modes, models, reasoning levels, and other controls that the ACP agent actually
advertises. A workflow step can combine all of them:

```yaml
- id: optimize
  agent: claude-site
  command: /loop
  prompt: Improve {{trigger.payload.page}} until the checks pass.
  session_config:
    mode: build
    thought_level: high
  mcp_server: seo
  mcp_tool: audit_page
  mcp_arguments:
    url: "{{trigger.payload.page}}"
  new_session: false
```

ACP standardizes attaching MCP servers, agent-advertised slash commands, and
session configuration. It does not define a client-to-agent RPC for directly
calling an MCP tool. The `mcp_*` workflow fields therefore ask the native
agent to perform the tool call inside its turn, preserving the harness's
own permissions, hooks, and tool-call activity.

### Run Tests

```bash
# Rust tests
cargo test -p xpressclaw-core -p xpressclaw-server

# Frontend type check
cd frontend
npm run check
npx playwright install chromium # once per machine
npm run test:e2e
cd ..

# Formatting and linting
./scripts/rustfmt.sh --check
cargo clippy -p xpressclaw-core -p xpressclaw-server -p xpressclaw-cli -p xpressclaw-tauri --all-targets -- -D warnings
```

Use the formatting script instead of `cargo fmt --all`: Cargo's `--all` mode
also formats local path dependencies and would modify the
`external/ready-agent-cog` submodule.

### Development Mode

```bash
# Terminal 1: Run the Rust server with auto-reload
cargo run -- up

# Terminal 2: Run the frontend dev server with hot reload
cd frontend && npm run dev

# The frontend dev server proxies API calls to localhost:8935
```

## Architecture

xpressclaw is a Cargo workspace with four crates:

| Crate | Purpose |
|-------|---------|
| `xpressclaw-core` | Business logic: sessions, attempts, events, artifacts, tasks, workflows, SQLite, and worker isolation |
| `xpressclaw-server` | Axum REST API, SSE streaming, embedded SvelteKit frontend (rust-embed) |
| `xpressclaw-cli` | Local control-plane lifecycle and health commands |
| `xpressclaw-tauri` | Native desktop app with system tray (Tauri v2) |

```
xpressclaw
+-- Axum server (REST API + embedded SvelteKit frontend)
+-- Session event log (messages, attempts, provenance, artifacts)
+-- SQLite (sessions, events, tasks, workflows, and schedules)
+-- ACP client + task dispatcher (Codex / Claude Code / OpenCode / custom)
+-- Docker Manager (short-lived attempt containers)
```

**Key design decisions:**
- **Single binary** — server, API, frontend, and CLI in one executable
- **Container runtime required** — Docker and Podman are detected automatically; worker isolation is not optional
- **Durable local state** — sessions, task queues, schedules, and workflow runs survive restarts
- **ACP boundary** — Agents own their instructions, tools, reasoning loop, and subagents; xpressclaw owns tasks, orchestration, and presentation

## CLI Reference

```
xpressclaw init              Create an empty workspace configuration
xpressclaw up [--detach]     Start the control plane and worker dispatcher
xpressclaw down              Stop the control plane and active workers
xpressclaw status            Show logical session status
```

Default port: `8935` (override with `--port`).

Messages, tasks, schedules, workflows, results, and configuration live in the
web UI rather than a second CLI interface.

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

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

```bash
git clone https://github.com/XpressAI/xpressclaw.git
cd xpressclaw
./build.sh
```

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
