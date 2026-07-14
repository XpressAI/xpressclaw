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

Build the native worker image, open `http://localhost:8935`, and send work to a session:

```bash
docker build -t xpressclaw-native-runner:latest harnesses/native
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

### Persistent Memory

Zettelkasten-style knowledge base with vector search (sqlite-vec). Agents remember context across sessions and retrieve relevant information automatically.

### Multiple Native Runners

- **Codex:** reuses an eligible host ChatGPT/Codex login
- **Claude Code:** reuses an eligible host Claude subscription login
- **OpenCode:** JSON event adapter with configurable authentication
- **Custom:** one-argument-per-line command templates for other native CLIs

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
```bash
xpressclaw tasks create "Summarize top 10 HN stories" --agent atlas
```

**Review what happened while you were away:**
```bash
xpressclaw logs
xpressclaw status
xpressclaw budget
```

**Define SOPs for consistent behavior:**
```yaml
name: weekly-report
steps:
  - Check JIRA for completed tickets this week
  - Summarize key accomplishments
  - Identify blockers and risks
  - Draft report and send to team channel
```

**Interactive CLI chat:**
```bash
xpressclaw chat atlas
```

## Configuration

`xpressclaw init` creates a `xpressclaw.yaml` in your project:

```yaml
system:
  isolation: docker

agents:
  - name: atlas
    backend: codex
    runner:
      kind: codex
      image: xpressclaw-native-runner:latest
      subscription_auth: true
      max_turns: 100
    role: |
      You own implementation work in this repository.

memory:
  near_term_slots: 8
  eviction: least-recently-relevant

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

### Build the Native Worker Image

The default image contains Codex and Claude Code. OpenCode or another CLI can
be supplied in a custom image:

```bash
docker build -t xpressclaw-native-runner:latest harnesses/native
```

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
| `xpressclaw-cli` | Local control commands for setup, sessions, tasks, workflows, and compatibility features |
| `xpressclaw-tauri` | Native desktop app with system tray (Tauri v2) |

```
xpressclaw (single ~12MB binary)
+-- Axum server (REST API + embedded SvelteKit frontend)
+-- Session event log (messages, attempts, provenance, artifacts)
+-- SQLite + sqlite-vec (tasks, workflows, schedules, memory)
+-- Native worker dispatcher (Codex / Claude Code / OpenCode / custom)
+-- Docker Manager (short-lived attempt containers)
```

**Key design decisions:**
- **Single binary** — server, API, frontend, and CLI in one executable
- **Docker required** — worker isolation is not optional
- **SQLite for everything** — tasks, memory, embeddings, conversations, budget
- **Native agent ownership** — agent products own their reasoning loop and tool protocol

## CLI Reference

```
xpressclaw init              Initialize workspace with config + data dir
xpressclaw up [--detach]     Start the control plane and worker dispatcher
xpressclaw down              Stop the control plane and active workers
xpressclaw status            Show logical session status
xpressclaw chat <session>    Send messages through a logical session
xpressclaw tasks             Task management (list, create, update, delete)
xpressclaw memory            Memory inspection (list, search, add)
xpressclaw budget            Budget report and usage history
xpressclaw sop               SOP management (list, create, run)
xpressclaw logs              Activity log viewer
```

Default port: `8935` (override with `--port`).

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
