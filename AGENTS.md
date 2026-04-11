# Repository Guidelines

## Project Overview

Xpressclaw is an "Operating System for AI agents" — a runtime that makes it easy to run, manage, and observe AI agents. It handles isolation, memory, tools, budgets, and observability so you can focus on what your agents do, not how they run.

Core principles:
- **Zero Config Start**: `xpressclaw init && xpressclaw up` should just work
- **Framework Agnostic**: Run any agent backend (Claude SDK, OpenAI Codex, LangChain, local models) through a common interface
- **MCP Native**: All tools speak MCP (Model Context Protocol)
- **Observable**: Always know what agents are doing
- **Safety by Default**: Isolation, budgets, tool permissions

## Architecture & Data Flow

```
┌─────────────────────────────────────────────────┐
│           Tauri Desktop / CLI                    │
│  (native shell, system tray, sidecar launcher)  │
└───────────────────┬─────────────────────────────┘
                    │ spawns
┌───────────────────▼─────────────────────────────┐
│              xpressclaw server (Rust/Axum)       │
│  ┌──────────┐ ┌──────────┐ ┌─────────────────┐ │
│  │ LLM Proxy│ │ REST API │ │ Embedded SvelteKit│ │
│  │ /v1/     │ │ /api/    │ │ (rust-embed)     │ │
│  └────┬─────┘ └────┬─────┘ └─────────────────┘ │
│       │            │                             │
│  ┌────▼────────────▼──────────────────────────┐ │
│  │           xpressclaw-core                   │ │
│  │  agents · conversations · tasks · memory    │ │
│  │  budget · connectors · workflows · tools    │ │
│  └────────────────┬───────────────────────────┘ │
│                   │                              │
│  ┌────────────────▼───────────────────────────┐ │
│  │          SQLite + sqlite-vec               │ │
│  └────────────────────────────────────────────┘ │
└───────────────────┬─────────────────────────────┘
                    │ manages
        ┌───────────┼───────────┐
    ┌───▼───┐   ┌───▼───┐   ┌───▼───┐
    │Agent 1│   │Agent 2│   │Agent 3│
    │(env)  │   │(env)  │   │(env)  │
    └───────┘   └───────┘   └───────┘
```

**Message flow**: User → frontend → REST API → conversation_messages (DB) → processor → harness (agent env) → LLM proxy → model → tool calls → harness executes → response stored → SSE to frontend.

**LLM proxy**: The server acts as an Anthropic API proxy. Agent environments call `http://server:8935/v1/messages`. For Claude models, requests proxy to Anthropic. For local models, requests route through the LLM router (llama.cpp or Ollama).

## Key Directories

| Directory | Purpose |
|-----------|---------|
| `crates/xpressclaw-core/` | Business logic: agents, conversations, tasks, memory, budget, tools, config, DB |
| `crates/xpressclaw-server/` | Axum REST API + LLM proxy + embedded frontend (rust-embed) |
| `crates/xpressclaw-cli/` | CLI commands via clap (`xpressclaw up`, `init`, `chat`, etc.) |
| `crates/xpressclaw-tauri/` | Tauri v2 desktop app with system tray, launches CLI as sidecar |
| `frontend/` | SvelteKit 5 web UI (Svelte 5, Tailwind CSS), built as static SPA |
| `harnesses/` | Agent runtime environments — base server + backend-specific harnesses |
| `harnesses/base/` | Common MCP server (Python), shared across all harnesses |
| `harnesses/claude-sdk/` | Claude Agent SDK harness with persistent sessions |
| `docs/adr/` | Architecture Decision Records (read before making changes) |
| `external/ready-agent-cog/` | Git submodule for agent cognitive architecture |

## Development Commands

### Build & Run

```bash
# Full build (CLI + desktop + Docker harnesses + frontend)
./build.sh

# Quick server-only build (for iteration)
cargo build --release -p xpressclaw-cli

# Run the server directly
./target/release/xpressclaw up --port 8935

# Run the desktop app (builds and launches Tauri)
cargo run -p xpressclaw-tauri

# Frontend dev server (hot reload, proxies to backend)
cd frontend && npm run dev
```

### Test

```bash
# Rust tests
cargo test -p xpressclaw-core -p xpressclaw-server

# Frontend type check
cd frontend && npm run check
```

### Lint & Format

```bash
# Rust formatting (CI enforces this)
cargo fmt
cargo fmt -- --check

# Clippy
cargo clippy
```

### Docker Harness Images

```bash
# Build all harness images
docker build -t ghcr.io/xpressai/xpressclaw-harness-base:latest harnesses/base
docker build -t ghcr.io/xpressai/xpressclaw-harness-claude-sdk:latest harnesses/claude-sdk
```

## Code Conventions & Common Patterns

- **Rust**: Composition over inheritance. Small focused modules. Never swallow errors silently.
- **Error handling**: `thiserror` for error types in core, `anyhow` avoided. Log errors with context.
- **Async**: Tokio runtime throughout. `async_trait` for trait objects.
- **Database**: Raw SQL via rusqlite, not an ORM. Migrations in `db.rs` as `MIGRATION_V{N}` constants.
- **Frontend**: Svelte 5 runes (`$state`, `$derived`, `$effect`). Tailwind CSS. No component library.
- **API**: REST endpoints return JSON. SSE for streaming. WebSocket for real-time events.
- **Config**: YAML (`xpressclaw.yaml`) for user config. Environment variables for secrets. Sensible defaults for everything.
- **ADRs**: Read the relevant ADR in `docs/adr/` before changing any subsystem. There are 22 ADRs covering all major design decisions.

## Important Files

### Entry Points

| File | Purpose |
|------|---------|
| `crates/xpressclaw-cli/src/main.rs` | CLI entry point |
| `crates/xpressclaw-tauri/src/main.rs` | Desktop app entry point (launches CLI as sidecar) |
| `crates/xpressclaw-server/src/server.rs` | Server startup, route mounting, reconciler launch |
| `frontend/src/routes/+layout.svelte` | Frontend root layout (sidebar, navigation) |

### Core Modules

| Module | Purpose |
|--------|---------|
| `core/src/agents/` | Agent lifecycle, harness client, reconciler (desired-state controller) |
| `core/src/conversations/` | Message storage, processor (routes messages to agents), event bus |
| `core/src/tasks/` | Task board, dispatcher (assigns tasks to agents), SOPs |
| `core/src/memory/` | Zettelkasten notes, vector search (sqlite-vec), near-term slots |
| `core/src/budget/` | Per-agent spending limits, rate limiting, cost tracking |
| `core/src/llm/` | LLM router, providers (llama.cpp, Ollama, OpenAI), model management |
| `core/src/docker/` | Container management via bollard (Docker/Podman) |
| `core/src/config.rs` | YAML config parsing, defaults, validation |
| `core/src/db.rs` | SQLite schema, migrations, connection pool |
| `server/src/routes/llm.rs` | LLM proxy — Anthropic & OpenAI compatible endpoints |
| `server/src/routes/apps.rs` | Agent-published web apps |

### Configuration

| File | Purpose |
|------|---------|
| `~/.xpressclaw/xpressclaw.yaml` | User configuration (agents, tools, budget, isolation) |
| `~/.xpressclaw/xpressclaw.db` | SQLite database (all persistent state) |
| `.claude/settings.local.json` | Claude Code settings for this repo |

### Specification

- Architecture Decision Records: `docs/adr/ADR-*.md` (22 ADRs, numbered 000-022)
- Key ADRs: 001 (overview), 002 (agent backends), 003 (container isolation), 006 (SQLite), 015 (SvelteKit UI), 018 (desired-state controller), 021 (agent sessions), 022 (connectors/workflows)

## Runtime/Tooling Preferences

### Runtime Requirements

- Rust 1.91+ (pinned in CI for async_trait compatibility)
- Node.js 20+ (for frontend build and MCP server subprocess in harnesses)
- SQLite 3 (bundled via rusqlite)
- Docker/Podman (for agent isolation — being replaced by Wanix)

### Package Manager

- Rust: Cargo workspace (4 crates)
- Frontend: npm (SvelteKit, Tailwind)
- Python (harnesses): pip, installed in Docker images

### Tooling Constraints

- The server binary embeds the frontend via `rust-embed`. The frontend MUST be built before the server for production builds. The server's `build.rs` handles this automatically with `rerun-if-changed` directives.
- The desktop app launches the CLI binary as a sidecar. Both must be built to the same target directory. `build.sh` handles copying the CLI to the Tauri binaries path.
- Harness images are Docker images pushed to `ghcr.io/xpressai/`. They contain Python + the base MCP server + backend-specific code.

### Environment Variables

| Variable | Purpose |
|----------|---------|
| `ANTHROPIC_API_KEY` | Anthropic API key for Claude models |
| `OPENAI_API_KEY` | OpenAI API key (optional) |
| `XPRESSCLAW_PORT` | Server port (default: 8935) |
| `XPRESSCLAW_WORKDIR` | Data directory (default: `~/.xpressclaw`) |

## Testing Workflow & Philosophy

### TDD-First Approach

**All work follows Test-Driven Development:**

1. **Write tests first** - Before implementing any functionality, create tests that describe the expected behavior
2. **Tests must fail initially** - Verify tests actually exercise the code by running them before implementation
3. **Implement minimally** - Write only the code needed to make tests pass
4. **Refactor** - Clean up while keeping tests green

This applies to all new features, bug fixes, and refactoring.

### Test Types and Hierarchy

Tests are organized by scope:

| Test Type | Scope | I/O & Networking | When to Use |
|-----------|-------|------------------|-------------|
| **Unit tests** | Single unit (module/function) | Minimized | Prefer for most testing; verify contracts, edge cases, error handling |
| **Component tests** | Multiple units working together | Minimized | Test component boundaries, internal interfaces |
| **Integration tests** | Component integration | Required | Verify component interoperability, database operations |
| **End-to-End tests** | Full system | Full system | External-facing behavior, CLI operations |
| **Regression tests** | Bug reproduction | Varies | Created before bug fixes; must fail before fix, pass after |

**Test preferences:**
- **Prefer unit and component tests** - Limit I/O and networking to smallest amount possible
- **Limit integration tests** - Use only when no adequate unit/component solution exists
- **E2E tests in separate module** - Build in `tests/` directory, target configurable system

### Test Quality Standards

1. **Tests are first-class code** - Apply same care as application code; refactor reappearing patterns
2. **Tests should be readable** - Clear intent, understandable without reading implementation
3. **Tests stay implementation-independent** - Avoid tight coupling to implementation details
4. **Fast feedback** - Most tests run in seconds, not minutes
5. **Tests become more valuable over time** - Maintain tests to prevent rot and disablement

### Test Infrastructure

- Rust tests use `#[cfg(test)]` modules within source files and `tests/` directories
- SQLite tests use in-memory databases (`:memory:`)
- No mock frameworks — prefer real implementations with controlled inputs
- Frontend: `svelte-check` for type checking (no unit test framework currently)

### Test Frameworks

- Rust: built-in `#[test]`, `tokio::test` for async
- Frontend: `svelte-check` (type checking only)

### Test Structure

- Unit tests live in `#[cfg(test)] mod tests` at the bottom of each source file
- Integration tests in `crates/*/tests/` directories
- Frontend type checks via `npm run check`

### Key Invariants to Test

- Agent messages are stored before being processed (crash recovery)
- Budget limits are enforced before LLM calls (never overspend)
- Conversation event bus delivers all events (no dropped messages)
- MCP tool calls are routed to the correct server
- Config validation rejects invalid states early

## Development Philosophy

### Less, But Better

**Core principle: More code is a liability, not an asset.**

We implement features with the **minimal amount of code** necessary. This is not about being lazy or cutting corners—it's about recognizing that code is complexity waiting to happen.

**When issues arise:**

1. **Stop and reconsider** - What is this issue telling us? What category of problem does it represent?
2. **Solve the category** - Figure out how to solve for the entire class of problem, not just this instance
3. **Categorical fixes are often easier** - Patching one specific thing is frequently more complex than addressing the root cause

**Why this approach:**

- More code provides more space for complexity to fester
- More code creates more places for bugs to hide
- Each line added is a maintenance burden for future changes
- Categorical solutions are more robust and simpler to reason about

**Practical implications:**

- Prefer a clean abstraction over ad-hoc handling of edge cases
- When you find yourself adding special cases, reconsider the design
- A bug that requires 3 special cases to fix likely indicates a deeper design issue
- Refactoring to enable a simple solution is preferred over a complex patch

### Commit Messages

**After each feature is fully implemented or bug is fixed, commit with messages that explain both what and why.**

Commit messages should document:

- **What was done** - The change itself (visible in diff, but summarize)
- **Why it was done** - The reasoning, context, and tradeoffs considered

**Purpose:** Use `git blame` later to understand why code is the way it is. The commit message is the historical record of design decisions.

**Guidelines:**

- Explain the problem being solved, not just the solution
- Mention alternative approaches considered and why they were rejected
- Note any tradeoffs or compromises made
- Reference related issues, specs, or discussions
- Keep the "why" even when the "what" is obvious from the diff

## Design Principles

1. **Zero Config Start** — `xpressclaw init && xpressclaw up` should just work with no prerequisites
2. **Cloud Optional** — Default to local models, support cloud APIs when configured
3. **Safety by Default** — Isolation, budgets, tool permissions out of the box
4. **Observable** — Users should always be able to see what agents are doing
5. **Framework Agnostic** — Run any agent backend through a common interface
6. **MCP Native** — Tools speak MCP, period
7. **Agents Remember and Learn** — Zettelkasten memory with vector search

## Common Operations

```bash
# Initialize a workspace
xpressclaw init

# Start the server and agents
xpressclaw up [--detach]

# Interactive chat with an agent
xpressclaw chat <agent>

# Check agent status
xpressclaw status

# View agent logs
xpressclaw logs <agent>

# Task management
xpressclaw tasks list|create|update|delete

# Memory inspection
xpressclaw memory list|search|add
```
