# ADR-001: Project Overview and Goals

## Status
Accepted

## Context

The AI agent landscape has exploded with frameworks (LangChain, CrewAI, AutoGen, etc.) and powerful SDKs (Claude Agent SDK, OpenAI Codex). However, running agents in production remains complex:

- **No standard runtime**: Each framework has its own way of running agents
- **Isolation is DIY**: Agents can trash your filesystem or rack up costs
- **Memory is fragmented**: Every framework reinvents context management
- **Observability is an afterthought**: "What did my agent do while I slept?"
- **Configuration explosion**: Too many choices paralyze new users

Meanwhile, in the web world, Phusion Passenger solved a similar problem: it made deploying Ruby/Python apps trivially easy. Point Apache/Nginx at your app, and Passenger handled process management, restarts, and scaling.

## Decision

We will build **XpressAI**: an agent runtime that provides the infrastructure layer for running AI agents, regardless of which framework or SDK they're built with.

### Core Principles

1. **Zero Configuration Start**
   ```bash
   xpressai init
   xpressai up
   ```
   That's the entire getting-started experience. No API keys required (uses local model by default).

2. **Local First, Cloud When Ready**
   - Default to Qwen3-8B running locally (8GB GPU minimum)
   - Upgrade to Claude/GPT when you need more capability
   - Agent can even suggest the upgrade when it's struggling

3. **Framework Agnostic**
   - XpressAI is not another agent framework
   - It's a runtime that runs agents built with any framework
   - Common interface for Claude Agent SDK, LangChain, CrewAI, Aider, etc.

4. **Safety by Default**
   - Agents run in Docker containers (isolation)
   - Budget limits with configurable enforcement
   - Tool permissions are explicit
   - Rate limiting built-in

5. **Observable**
   - Know what agents did, when, and why
   - Memory inspection
   - Cost tracking
   - Activity logs

### Product Scope

**In Scope (MVP)**:
- Agent runtime with container isolation
- Claude Agent SDK as primary backend (for developer agent teams)
- Local model support (Qwen3-8B)
- Memory system (zettelkasten + vector search)
- MCP-based tool system
- Task/SOP management
- Budget controls
- CLI, TUI, and simple web dashboard

**Out of Scope (for now)**:
- Multi-tenant SaaS
- Custom model training
- Visual workflow builders
- Marketplace for agents/tools

## Consequences

### Positive
- Clear product positioning (runtime, not framework)
- Low barrier to entry (zero-config, local-first)
- Addresses real pain points (isolation, memory, observability)
- Can leverage existing frameworks rather than competing with them

### Negative
- Must maintain adapters for multiple agent backends
- Local model experience may underwhelm users expecting GPT-4 quality
- Docker dependency adds complexity for some users

### Risks
- Agent framework landscape is volatile; backends may change rapidly
- "Simple" can become "limited" if we're too opinionated
- Performance overhead from container isolation

## Notes

The name "XpressAI" connects to Xpress AI (Eduardo's company) while suggesting speed and simplicity. It positions as the express lane to running agents.
