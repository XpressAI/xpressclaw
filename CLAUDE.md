# Xpressclaw - AI Agent Runtime

## BEFORE YOU DO ANYTHING — Read Git Notes

You do not "know" — you predict. The git notes are your memory. **Read them first.**

```bash
# Read notes on recent commits to understand WHY things are the way they are
git log --notes=refs/notes/agent --format="%H %s%n%N" -10
```

Do this at the start of every task and after every context compaction. Without this context, you are guessing — and guessing produces bad software.

**After every commit, write a note:**

```bash
git notes --ref=refs/notes/agent add -m "your note here" HEAD
```

Capture: what you were thinking, why you made the choices you did, what alternatives you rejected, and anything a future agent would need to avoid re-learning the hard way. A commit without a note is context lost forever.

See `docs/adr/ADR-000-version-control-workflow.md` for the full workflow.

## How to Work in This Repo

Stop guessing. Read the code, add logging, run the build, and inspect real values. Minimize surface area of changes. Prefer tiny, observable steps with a single hypothesis per step.

### Prime Directives

1. **Seek evidence first** — **Seek evidence first** I mean it! Before changing logic, log or print the values you're relying on. When something is broken, the correct tokens are no longer the most probable tokens; you must acquire evidence.
2. **Honor existing architecture** — Read the ADRs in `docs/adr/` and this README. Preserve public APIs; change internals surgically.
3. **Make state reconstructable** — Deterministic keys and context reconstruction prevent "ctx is null" classes of bugs. Avoid hidden global configuration; prefer per-request/per-entity data.
4. **Read documentation before guessing** — When integrating with external libraries, SDKs, or APIs, **always** read their official documentation first. Use WebFetch to pull docs pages. Do not assume how an API works based on parameter names or type signatures — read the actual docs, examples, and changelogs. This applies to: Claude Agent SDK, Tauri, Bazel, MCP protocol, Docker API, any npm/crate dependency. Your training data is stale; the docs are current. Every hour spent debugging a wrong assumption could have been avoided by 5 minutes of reading docs.

### Safe Change Process

1. Read git notes on recent commits (see top of this file)
2. Read the relevant ADRs for the area you're changing
3. Add precise logs. Reproduce. Capture evidence.
4. Make a tiny change. Re-run. Compare output.
5. Only then expand scope.
6. After committing, write your git note. Always.

### What Goes Wrong Without This

- Assuming state or config will be ready; when it isn't, writing more assumptions on top.
- Not inspecting actual values before changing logic.
- Not persisting or reconstructing context deterministically.
- Making large changes without verifying each step.

If a value is unexpected, **stop and log which one**. Don't guess. One change, one measurement.

### Comprehensive Changes Only

When implementing something, do it completely across ALL platforms, ALL code paths, and ALL configurations. No half-implementations. If you add Bazel support for macOS, add it for Windows too. If you change a protocol, change ALL servers using that protocol. If you fix a bug in one route, check for the same bug in all routes. A partial implementation is worse than no implementation — it creates inconsistency and "works on my machine" problems. Before marking a task done, ask: "Did I handle every variant? Every platform? Every edge case?"

### No AI Branding

Do not add "Co-Authored-By" lines, "Generated with" footers, or any AI tool references to commits, PRs, or any externally-visible output. Ever.

---

[Xpressclaw](https://github.com/XpressAI/xpressclaw) is an "Operating System for AI agents" - a simple, opinionated runtime that makes it easy to run, manage, and observe AI agents. Think of it as an agent operating system that handles isolation, memory, tools, budgets, and observability so you can focus on what your agents do, not how they run.

## Project Vision

The goal is **radical simplicity** for getting agents running:

```bash
xpressclaw init
xpressclaw up
```

That's it. Zero configuration to start. Progressive disclosure for power users.

## Core Architecture

### Agent Backends
XpressAI is a runtime, not another agent framework. It runs agents built with:
- **Claude Agent SDK** (primary, especially for developer agent teams)
- OpenAI Codex
- Gemini CLI
- Aider
- LangChain
- CrewAI
- Xaibo (our framework)
- Local models via Qwen3-8B (default for zero-config start)

### Key Subsystems

1. **Container Isolation** - Each agent runs in a c2w-compiled WASM guest on wasmtime (ADR-023). Docker was removed.
2. **Memory System** - Zettelkasten-style notes with vector search (sqlite-vec), 8-slot near-term memory with eviction
3. **Tool System** - MCP (Model Context Protocol) as the universal standard for all tools
4. **Task System** - Kanban-style task board, SOPs that agents can create and follow
5. **Budget Controls** - Per-agent and global spending limits with configurable actions on exceed
6. **Observability** - What did my agent do at 3am?

### UI Layers
- **CLI** - Primary interface, `xpressclaw` command
- **Web UI** - SvelteKit SPA, embedded via rust-embed
- **Desktop** - Tauri-based native app with system tray

## Tech Stack

- **Language**: Rust
- **Database**: SQLite + sqlite-vec for vector storage
- **Container Runtime**: wasmtime + container2wasm (ADR-023). No Docker dependency.
- **Web Framework**: Axum + SvelteKit (static SPA embedded via rust-embed)
- **Desktop**: Tauri v2 with system tray
- **Agent SDK**: claude-agent-sdk (via containers)
- **Local Model**: Qwen3-8B via llama-cpp-2 or Ollama

## Project Structure

```
xpressclaw/                  # Project root
├── crates/                  # Rust workspace
│   ├── xpressclaw-core/     # Business logic (config, DB, agents, memory, tasks, budget, tools)
│   ├── xpressclaw-server/   # Axum REST API + embedded frontend (rust-embed)
│   ├── xpressclaw-cli/      # CLI commands via clap
│   └── xpressclaw-tauri/    # Native desktop app (Tauri v2)
├── frontend/                # SvelteKit web UI (Svelte 5, Tailwind CSS)
├── tests/
├── docs/
│   └── adr/                 # Architecture Decision Records
├── Cargo.toml               # Rust workspace root
└── README.md
```

## Development Guidelines

### Git Version Control
See the top of this file. Read notes first, write notes after every commit. Non-negotiable.

### Code Style
- Prefer composition over inheritance
- Keep modules focused and small

### Error Handling
- Never swallow errors silently
- Log errors with context

### Configuration
- Environment variables for secrets (API keys)
- YAML for user configuration (`xpressai.yaml`)
- Sensible defaults for everything
- Progressive disclosure: start simple, add complexity as needed

### Testing
- Integration tests with real containers where appropriate

## Key Design Principles

1. **Zero Config Start** - `xpressai init && xpressai up` should just work
2. **Cloud Optional** - We default to the Claude Agent SDK but also allow local models (like Qwen3-8B by default) when needed for privacy/security.
3. **Safety by Default** - Containers, budgets, tool permissions
4. **Observable** - Always know what agents are doing
5. **Framework Agnostic** - Run any agent backend through a common interface
6. **MCP Native** - Tools speak MCP, period
7. **Reliable** - SOPs ensure Agents perform consistently and can learn to avoid mistakes over time.
8. **Agents Remember and Learn** - The plain-text zettelcasten memory system with SQLite-vec powered enable agents that can learn.

## Current Focus

We're building the MVP with this priority:
1. Core runtime and configuration
2. Claude Agent SDK integration (for developer agent teams)
3. Docker isolation
4. SQLite + sqlite-vec memory
5. Basic CLI (init, up, status, logs)
6. SvelteKit web dashboard

## Commands Reference

```bash
xpressclaw init              # Initialize workspace with config + data dir
xpressclaw up [--detach]     # Start the server and agents
xpressclaw down              # Stop all running agents
xpressclaw status            # Show agent status and budget summary
xpressclaw chat <agent>      # Interactive chat in the terminal
xpressclaw tasks             # Task management (list, create, update, delete)
xpressclaw memory            # Memory inspection (list, search, add)
xpressclaw budget            # Budget report and usage history
xpressclaw sop               # SOP management (list, create, run)
xpressclaw logs              # Activity log viewer
```

## Configuration Example

```yaml
# xpressclaw.yaml - generated by `xpressclaw init`

system:
  budget:
    daily: $20.00
    on_exceeded: pause  # pause | alert | degrade | stop

  isolation: docker  # docker | none

agents:
  - name: atlas
    backend: generic  # or: claude-sdk, openai-codex, etc.
    role: |
      You are a helpful assistant...
    autonomy: high
    wake_on:
      - schedule: "every 30 minutes"
      - event: user.message

tools:
  builtin:
    filesystem: ~/agent-workspace
    web_browser: true
    shell:
      enabled: true
      allowed_commands: [git, npm, python]

memory:
  near_term_slots: 8
  eviction: least-recently-relevant
  # cleanup: none | delete_after:30d | summarize
```

## Reading the ADRs

Before making significant changes, read the relevant ADRs in `docs/adr/`:
- ADR-001: Project overview
- ADR-002: Agent backend abstraction
- ADR-003: Container isolation
- ADR-004: Memory system
- ADR-005: MCP tool system
- ADR-006: SQLite storage
- ADR-007: Web UI
- ADR-008: Textual TUI (legacy, replaced by web UI)
- ADR-009: Task/SOP system
- ADR-010: Budget controls
- ADR-011: Default local model

## Seek evidence first

**Seek evidence first** **Seek evidence first** **Seek evidence first**