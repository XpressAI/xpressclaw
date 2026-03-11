<p align="center">
  <img src="https://raw.githubusercontent.com/XpressAI/xpressclaw/main/docs/assets/xpressclaw-banner.png" alt="xpressclaw" width="600">
</p>

<h3 align="center">Your AI agents. Running while you sleep.</h3>

<p align="center">
xpressclaw is an open-source AI agent runtime. Define agents in YAML, give them tasks, and let them work autonomously — with built-in memory, budget controls, scheduling, and observability.
</p>

<p align="center">
<a href="https://xpressclaw.ai">Website</a> •
<a href="https://hub.xpressclaw.ai">Hub</a> •
<a href="https://github.com/XpressAI/xpressclaw/blob/main/CONTRIBUTING.md">Contribute</a> •
<a href="https://discord.com/invite/vgEg2ZtxCw">Discord</a>
</p>

<p align="center">
<a href="https://github.com/XpressAI/xpressclaw/blob/main/LICENSE"><img src="https://img.shields.io/github/license/XpressAI/xpressclaw?color=brightgreen" alt="License"></a>
<a href="https://github.com/XpressAI/xpressclaw/releases"><img src="https://img.shields.io/github/v/release/XpressAI/xpressclaw?color=yellow" alt="Release"></a>
<img src="https://img.shields.io/badge/python-3.11+-blue" alt="Python">
</p>

---

```bash
pip install xpressai
xpressai init
xpressai up
```

That's it. Your first agent is running.

> **Built before the hype.** The agent orchestration technology behind xpressclaw has been running in production at regulated financial institutions since 2024 — before "AI agents" became a buzzword. We open-sourced the core so everyone can benefit.

## Features

### Autonomous Agents

Agents run continuously, picking up tasks from a queue and working through them. Assign work and come back to results — no babysitting required.

### SOPs (Standard Operating Procedures)

Define step-by-step procedures your agents follow. Agents perform consistently, learn from mistakes, and improve over time. Predictable, auditable, reliable.

### Persistent Memory

Zettelkasten-style memory with vector search. Agents remember context across sessions, build knowledge over time, and retrieve relevant information automatically.

### Privacy & Safety by Default

- **Container isolation** — each agent runs in its own Docker container
- **Budget controls** — set daily spending limits so agents don't run up your API bill
- **Tool permissions** — explicit allow-list per agent; agents only get what they need
- **Local model support** — run Qwen3-8B, Llama 3, Mistral, and more via Ollama or llama.cpp

### Multiple Backends

- **Local:** Qwen3-8B (default, zero-config), any GGUF model via llama.cpp or Ollama
- **Cloud:** Claude, OpenAI, Gemini, and 100+ models via OpenRouter (bring your own key)
- **Framework agnostic:** Run agents built with Claude SDK, LangChain, CrewAI, Xaibo, and more

### Full Observability

Always know what your agents are doing. Logs, status dashboards, budget tracking, and a terminal UI for real-time monitoring.

### Community Hub

Discover and share agent configurations, SOPs, and skill packs at [hub.xpressclaw.ai](https://hub.xpressclaw.ai). Install community-built agents with one command.

## Quick Start

### Option 1: pip install

```bash
pip install xpressai
xpressai init
xpressai up
```

### Option 2: From Source

```bash
git clone https://github.com/XpressAI/xpressclaw.git
cd xpressclaw
pip install -e .
xpressai init
xpressai up
```

### Option 3: Native App (macOS / Windows)

Download from [xpressclaw.ai](https://xpressclaw.ai) — no terminal needed.

### Requirements

- Python 3.11+
- Docker (optional, for container isolation)
- An API key for your chosen backend (Claude, OpenAI, etc.) or a local model

## What Can It Do?

**Schedule recurring tasks:**
```bash
xpressai tasks atlas schedule "Summarize top 10 HN stories" --cron "0 9 * * *"
```

**Assign work and let agents handle it:**
```bash
xpressai tasks atlas add "Refactor the auth module to use JWT"
xpressai up -d
```

**Review what happened while you were away:**
```bash
xpressai logs atlas
xpressai status
xpressai budget
```

**Define an SOP for consistent behavior:**
```yaml
name: weekly-report
steps:
  - Check JIRA for completed tickets this week
  - Summarize key accomplishments
  - Identify blockers and risks
  - Draft report and send to team channel
```

## Configuration

`xpressai init` creates an `xpressai.yaml` in your project:

```yaml
system:
  budget:
    daily: $20.00
    on_exceeded: pause

  isolation: docker

agents:
  - name: atlas
    backend: claude-code
    role: |
      You are a helpful assistant.

tools:
  builtin:
    filesystem: ~/agent-workspace
    shell:
      enabled: true
      allowed_commands: [git, npm, python]

memory:
  near_term_slots: 8
  eviction: least-recently-relevant
```

## Architecture

**Tech Stack:**

| Component | Technology |
|-----------|-----------|
| Agent Runtime | Python 3.11+ |
| Web UI | FastAPI + HTMX (server-rendered dashboard) |
| Terminal UI | Textual (real-time monitoring) |
| Storage | SQLite + sqlite-vec (state, memory, embeddings) |
| Isolation | Docker (agent sandboxing) |
| Local LLM | llama.cpp / Ollama (privacy-first inference) |

**Key Subsystems:**

- **Agent Manager** — orchestrates agent lifecycles, handles restarts, manages temporary specialist agents
- **Memory System** — zettelkasten notes with vector search, 8-slot near-term memory with eviction
- **Tool System** — MCP (Model Context Protocol) as universal standard for all tools
- **Budget System** — per-agent and global spending limits with configurable actions
- **Isolation System** — Docker containers for each agent, filesystem and network sandboxing

## From Open Source to Enterprise

xpressclaw is the open-source foundation. When your organization needs team collaboration, visual workflows, compliance certifications, and enterprise support — Xpress AI has you covered.

| | xpressclaw (Free) | Xpress AI (Enterprise) |
|---|---|---|
| Autonomous AI agents | :white_check_mark: | :white_check_mark: |
| SOPs & scheduling | :white_check_mark: | :white_check_mark: |
| Local model support | :white_check_mark: | :white_check_mark: |
| Budget controls | :white_check_mark: | :white_check_mark: |
| Team collaboration | | :white_check_mark: |
| Visual workflow builder (Xircuits) | | :white_check_mark: |
| On-premise deployment | | :white_check_mark: |
| SOC 2 Type I (in progress) | | :white_check_mark: |
| Role-based access control | | :white_check_mark: |
| Audit logging & compliance | | :white_check_mark: |
| Dedicated support & SLA | | :white_check_mark: |

[Request an Enterprise Demo](https://xpress.ai)

## The Story Behind xpressclaw

We didn't start building AI agents because it was trendy. We started because our enterprise customers — regulated financial institutions in Japan — needed AI that could actually be deployed.

That meant:
- **On-premise** (no data leaving the building)
- **Auditable** (every agent action logged)
- **Reliable** (SOPs, not prompt-and-pray)

We built this technology as Xpress AI — and it's been running in production at companies like Moneytree (a MUFG subsidiary) since before "AI agents" became a buzzword.

Now we're open-sourcing the core as xpressclaw because we believe everyone deserves AI agents that actually work.

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

```bash
git clone https://github.com/XpressAI/xpressclaw.git
cd xpressclaw
pip install -e ".[dev]"
pre-commit install
pytest
```

## Community

- **Website:** [xpressclaw.ai](https://xpressclaw.ai)
- **Hub:** [hub.xpressclaw.ai](https://hub.xpressclaw.ai)
- **Discord:** [discord.com/invite/vgEg2ZtxCw](https://discord.com/invite/vgEg2ZtxCw)
- **Twitter/X:** [@xpressclaw](https://twitter.com/xpressclaw)
- **Enterprise inquiries:** [enterprise@xpress.ai](mailto:enterprise@xpress.ai)

## License

[Apache-2.0](LICENSE)

---

<p align="center">
Built with love by the <a href="https://xpress.ai">Xpress AI</a> team.<br>
<i>The AI workforce your compliance team will actually approve.</i>
</p>
