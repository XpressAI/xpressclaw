<p align="center">
  <img src="https://github.com/XpressAI/xpressclaw/blob/7a455d7bf77caf6dafdead4d37c79c7e3f6be809/docs/assets/xpressclaw-banner.jpeg" alt="xpressclaw" width="600">
</p>

<h3 align="center">Collaborative Local Agent Workspace</h3>

<p align="center">
A single binary that gives you a complete AI agent runtime — chat with your agents, give them tasks, and let them work autonomously. Built-in memory, budget controls, scheduling, and a polished web UI.
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

That's it. Open `http://localhost:8935` and start chatting with your agents.

## Why xpressclaw?

Most agent frameworks give you a library. xpressclaw gives you a **running system** — a ~12MB binary with everything included: server, web UI, LLM router, and agent management. No Python environment to configure, no Docker Compose sprawl, no YAML templating engines.

- **Chat-first interface** — Talk to your agents in a messaging UI, not a terminal. `@mention` agents in conversations, just like Slack.
- **Single binary, zero dependencies** — Download one file, run it. The server, API, and web frontend are all embedded.
- **Native desktop app** — Tauri-based `.app` / `.dmg` with system tray. Runs in the background, always available.
- **Production-tested architecture** — Built on the same agent orchestration patterns that power Xpress AI's enterprise platform, deployed at regulated financial institutions.
- **Local-first, cloud-optional** — Works with Ollama out of the box. Add OpenAI or Anthropic keys when you need them.
- **Secure by default** — Agents run in Docker containers. Budget controls prevent runaway costs. No exceptions.

## Features

### Chat with Your Agents

The primary interface is a **messaging UI**. Create conversations, add agents, and talk to them — individually or in groups. Agents respond via the configured LLM (local or cloud).

### Autonomous Task Execution

Agents pick up tasks from a queue and work through them. Schedule recurring work with cron expressions. Define SOPs (Standard Operating Procedures) so agents perform consistently.

### Persistent Memory

Zettelkasten-style knowledge base with vector search (sqlite-vec). Agents remember context across sessions and retrieve relevant information automatically.

### Multiple LLM Backends

- **Local:** Qwen 3.5, Llama 3, Mistral, and more via Ollama
- **Cloud:** Claude (Anthropic), GPT-4o (OpenAI), and 100+ models via OpenRouter
- **Framework agnostic:** Agent harnesses for Claude SDK, LangChain, Xaibo, and generic

### Privacy & Safety

- **Container isolation** — each agent runs in its own Docker container
- **Budget controls** — daily/monthly spending limits per agent and globally
- **Tool permissions** — explicit allow-list; agents only access what you grant
- **Everything local** — your data never leaves your machine unless you choose a cloud LLM

### Full Observability

Activity logs, budget dashboards, agent status monitoring. Know what your agents did at 3am.

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

- Docker or Podman (required for agent container isolation)
- Ollama (optional, for local LLM — `ollama pull qwen3.5:latest`)
- Or an API key for Claude / OpenAI / OpenRouter

## What Can It Do?

**Chat with agents from the web UI:**

Create a conversation, add an agent, and start talking. Use `@atlas` to mention a specific agent in a multi-agent conversation.

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
  budget:
    daily: $20.00
    on_exceeded: pause
  isolation: docker

agents:
  - name: atlas
    backend: generic
    role: |
      You are a helpful assistant.

memory:
  near_term_slots: 8
  eviction: least-recently-relevant

llm:
  default_provider: local
  # local_model: qwen3.5:latest
  # Set OPENAI_API_KEY or ANTHROPIC_API_KEY env vars for cloud providers
```

## Building

### Prerequisites

- [Bazel](https://bazel.build/) 8.2+ (via [Bazelisk](https://github.com/bazelbuild/bazelisk))
- [Rust](https://rustup.rs/) (stable toolchain, used by Bazel and for fmt/clippy)
- [LLVM](https://releases.llvm.org/) (provides `libclang`, required by llama.cpp bindings)
- [CMake](https://cmake.org/) (required by llama.cpp build)
- [Node.js](https://nodejs.org/) 18+ (for the frontend)
- Docker (for running agents)

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

### Build Agent Harness Images

Agent harnesses are Docker images that run your agents in isolation:

```bash
cd harnesses

# Build all harness images
docker buildx bake

# Or build individually
docker build -t xpressclaw-harness-base ./base
docker build -t xpressclaw-harness-generic ./generic
docker build -t xpressclaw-harness-claude-sdk ./claude-sdk
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
| `xpressclaw-core` | Business logic: config, SQLite + sqlite-vec, agents, memory, tasks, budget, LLM router, Docker management, MCP tools |
| `xpressclaw-server` | Axum REST API, SSE streaming, embedded SvelteKit frontend (rust-embed) |
| `xpressclaw-cli` | 10 CLI commands via clap: init, up, down, status, chat, tasks, memory, budget, sop, logs |
| `xpressclaw-tauri` | Native desktop app with system tray (Tauri v2) |

```
xpressclaw (single ~12MB binary)
+-- Axum server (REST API + embedded SvelteKit frontend)
+-- LLM Router (Ollama / OpenAI / Anthropic)
+-- SQLite + sqlite-vec (tasks, memory, conversations, budget)
+-- Docker Manager (agent container lifecycle)
+-- Agent Harnesses (isolated Python containers per backend)
```

**Key design decisions:**
- **Single binary** — server, API, frontend, and CLI in one executable
- **Docker required** — agent isolation is not optional
- **SQLite for everything** — tasks, memory, embeddings, conversations, budget
- **OpenAI-compatible protocol** — harnesses expose `/v1/chat/completions`

## CLI Reference

```
xpressclaw init              Initialize workspace with config + data dir
xpressclaw up [--detach]     Start the server and agents
xpressclaw down              Stop all running agents
xpressclaw status            Show agent status and budget summary
xpressclaw chat <agent>      Interactive chat in the terminal
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
