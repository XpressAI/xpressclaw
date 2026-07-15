<p align="center">
  <img src="https://github.com/XpressAI/xpressclaw/blob/7a455d7bf77caf6dafdead4d37c79c7e3f6be809/docs/assets/xpressclaw-banner.jpeg" alt="xpressclaw" width="600">
</p>

<h3 align="center">Control Plane for Native Agent Work</h3>

<p align="center">
Run Codex, Claude Code, OpenCode, and other native agents as isolated workers. Queue tasks, schedule recurring work, coordinate multi-agent workflows, and follow everything through one structured session UI.
</p>

<div align="center">
  <img width="612" height="426" alt="XpressClaw-screenshot" src="https://github.com/user-attachments/assets/e38079ef-99f7-4e1e-91a0-fa14d39800ca" />
</div>

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

Open `http://localhost:8935`, create a session, choose a project workspace,
and send it work. The matching runner image is pulled automatically. To build
the Codex runner locally instead:

```bash
docker build -t ghcr.io/xpressai/xpressclaw-runner-codex:latest harnesses/native/codex
```

## Why xpressclaw?

Codex, Claude Code, and OpenCode already supply excellent agent loops. xpressclaw is the **control plane around them**: durable work, automation, isolation, devices, and a UI for outcomes rather than terminals.

- **One logical session** — People, schedules, connectors, workflows, and native workers contribute to one event history.
- **Native workers** — The selected CLI owns reasoning and tools; xpressclaw does not put another agent framework in front of it.
- **Structured interface** — See tasks, attempts, artifacts, questions, and review decisions without watching a terminal.
- **Native desktop app** — Tauri-based `.app` / `.dmg` with system tray. Runs in the background, always available.
- **Automation-first** — Queue tasks, run recurring schedules, and express implementation/review loops as workflows.
- **Isolated by default** — Every native invocation runs in a short-lived Docker/Podman container.

## Features

### Persistent Sessions

The primary interface is a durable event timeline. It accepts a new message while work is running, records where every event came from, and keeps native execution contexts separate from the user-facing identity.

### Autonomous Task Execution

Native workers pick up tasks from a queue and publish structured progress and artifacts. Schedule recurring work with cron expressions. Workflows coordinate different products—for example, Codex implementing and Claude reviewing in a loop.

### Multiple Native Runners

- **Codex:** reuses an eligible host ChatGPT/Codex login
- **Claude Code:** reuses an eligible host Claude subscription login
- **OpenCode:** JSON event adapter with configurable authentication
- **Custom:** one-argument-per-line command templates for other native CLIs

Each built-in runner image contains one agent product. Language SDKs and
project services belong in a separate development environment rather than in
the agent identity image.

### Privacy & Safety

- **Container isolation** — each work attempt runs in its own short-lived container
- **Explicit resources** — workers receive the configured workspace and volume mounts
- **Visible provenance** — every queued request records whether it came from a person, schedule, connector, task, or workflow
- **Local control data** — timelines and task state remain local; native CLI traffic follows the selected provider's terms
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

### Option 2: Native App (macOS)

Download `xpressclaw.dmg` from [Releases](https://github.com/XpressAI/xpressclaw/releases) — double-click to install. The app runs in the system tray.

### Option 3: Build from Source

See [Building](#building) below.

### Requirements

- Docker or Podman (required for worker isolation)
- At least one supported native CLI login on the host
- The included native worker image, or your own compatible image

## What Can It Do?

**Send work from the session UI:**

Open a persistent session and describe the outcome. The request becomes a task and native work attempt while the UI remains available.

**Schedule recurring tasks:**

Create a task in **Work → Tasks**, select its session, then add a cron schedule
in **Work → Schedules**. Scheduled work enters the same queue and timeline as a
message sent by a person.

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

- [Bazel](https://bazel.build/) 8.2+ (via [Bazelisk](https://github.com/bazelbuild/bazelisk))
- [Rust](https://rustup.rs/) (stable toolchain, used by Bazel and for fmt/clippy)
- [LLVM](https://releases.llvm.org/) (provides `libclang`, required by llama.cpp bindings)
- [CMake](https://cmake.org/) (required by llama.cpp build)
- [Node.js](https://nodejs.org/) 18+ (for the frontend)
- Docker (for native workers)

### Build Everything

```bash
git clone https://github.com/XpressAI/xpressclaw.git
cd xpressclaw

# Build CLI, core, and server (includes frontend)
./build.sh

# Or with a clean build
./build.sh --clean
```

### Build Individual Targets

```bash
# CLI only
bazel build //crates/xpressclaw-cli:xpressclaw

# Core library
bazel build //crates/xpressclaw-core:xpressclaw-core

# Server
bazel build //crates/xpressclaw-server:xpressclaw-server

# The CLI binary is at bazel-bin/crates/xpressclaw-cli/xpressclaw
```

### Build the Desktop App (Tauri)

```bash
# Build everything including the Tauri desktop app
./build.sh

# For signed/notarized macOS builds
./build-signed.sh
```

### Build Native Worker Images

Build only the product you use. Each image is deliberately independent so it
can be versioned or extended without pulling in the other agent CLIs:

```bash
docker build -t ghcr.io/xpressai/xpressclaw-runner-codex:latest harnesses/native/codex
docker build -t ghcr.io/xpressai/xpressclaw-runner-claude:latest harnesses/native/claude
docker build -t ghcr.io/xpressai/xpressclaw-runner-opencode:latest harnesses/native/opencode
```

See `harnesses/native/README.md` for customization guidance and the planned
separation between agent runners and development environments.

### Run Tests

```bash
# Via Bazel
bazel test //crates/xpressclaw-core:core_test //crates/xpressclaw-server:server_test

# Frontend type check
cd frontend && npm run check

# Formatting and linting (still via Cargo)
cargo fmt -p xpressclaw-core -p xpressclaw-server -p xpressclaw-cli -p xpressclaw-tauri -- --check
cargo clippy -p xpressclaw-core -p xpressclaw-server -p xpressclaw-cli -p xpressclaw-tauri --all-targets -- -D warnings
```

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
xpressclaw (single ~12MB binary)
+-- Axum server (REST API + embedded SvelteKit frontend)
+-- Session event log (messages, attempts, provenance, artifacts)
+-- SQLite (sessions, events, tasks, workflows, and schedules)
+-- Native worker dispatcher (Codex / Claude Code / OpenCode / custom)
+-- Docker Manager (short-lived attempt containers)
```

**Key design decisions:**
- **Single binary** — server, API, frontend, and CLI in one executable
- **Docker required** — worker isolation is not optional
- **Durable local state** — sessions, task queues, schedules, and workflow runs survive restarts
- **Native agent ownership** — agent products own their reasoning loop and tool protocol

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
