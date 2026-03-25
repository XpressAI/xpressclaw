# ADR-016: Navigation Restructure

## Status
Accepted

## Context

The current sidebar navigation grew organically and doesn't group features logically. The bottom tab bar has Apps/Tasks/Schedules/Budget — four unrelated items. The sidebar mixes conversations, agents, and secondary nav links (Knowledge, Procedures, Settings) in a flat list. Users have to hunt for related features across different sections.

The navigation needs to reflect how users actually think about the system:
- **Agents** are the core — conversations and knowledge are agent activities
- **Tasks** and schedules are related (both about work management)
- **Procedures** are about automation workflows
- **Settings** encompasses configuration, budgets, and system info

## Decision

Restructure navigation into four top-level tabs in the bottom bar, each containing related features:

### Tab Structure

```
┌─────────────┬─────────────┬─────────────┬─────────────┐
│   Agents    │    Tasks    │  Workflows  │  Settings   │
└─────────────┴─────────────┴─────────────┴─────────────┘
```

#### 1. Agents (default tab)
The primary view. Contains everything about agents and interacting with them.

**Sidebar content:**
- **Apps** section — Dashboard and future agent-published apps
- **Conversations** section — Multi-participant chat sessions (with + button)
- **Agents** section — Agent list with status rings (with + button)
- **Knowledge** link — Memory/zettelkasten browser

This is the view users spend most time in. Chatting with agents, monitoring their status, and browsing their knowledge.

#### 2. Tasks
Work management. Contains tasks and their scheduling.

**Sidebar content:**
- **Tasks** — Kanban board (pending/in_progress/completed)
- **Schedules** — Cron jobs and recurring task definitions

Tasks and schedules are tightly coupled — schedules create tasks, tasks can be viewed alongside their triggers.

#### 3. Workflows
Automation and orchestration. Contains procedures and future workflow features.

**Sidebar content:**
- **Procedures** — SOP library (existing)
- **Workflows** — Agent-to-agent workflow definitions (future feature)

Procedures are deterministic step-by-step plans. Workflows will define how agents collaborate — handoffs, approvals, escalations. Both are about automating multi-step processes.

#### 4. Settings
All configuration in one place.

**Sidebar content:**
- **Profile** — User name and avatar
- **Server** — Address, ports, version, status
- **LLM Providers** — Default provider settings, API keys, base URLs, local model config
- **Model Pricing** — Custom cost tables for budget tracking
- **Budgets** — Global and per-agent budget configuration
- **Connectors** — MCP server configuration (future: external integrations)

Currently settings is a single page. This tab turns it into a proper configuration hub where each section can expand as features grow.

### Sidebar Behavior

The sidebar content changes based on which tab is active:

- **Agents tab**: Shows the current layout (apps, conversations, agents list, knowledge)
- **Tasks tab**: Shows task filters/views and schedule list
- **Workflows tab**: Shows procedure list and workflow list
- **Settings tab**: Shows settings categories as nav links

The main content area renders the selected item from the sidebar.

### URL Structure

```
/                          → Agents tab, new conversation
/dashboard                 → Agents tab, dashboard app
/conversations/{id}        → Agents tab, conversation view
/agents/{id}               → Agents tab, agent config
/memory                    → Agents tab, knowledge browser

/tasks                     → Tasks tab, task board
/tasks/{id}                → Tasks tab, task detail
/schedules                 → Tasks tab, schedule list

/procedures                → Workflows tab, procedure list
/procedures/{id}           → Workflows tab, procedure detail
/workflows                 → Workflows tab, workflow list (future)

/settings                  → Settings tab, profile
/settings/server           → Settings tab, server info
/settings/llm              → Settings tab, LLM providers
/settings/pricing          → Settings tab, model pricing
/settings/budget           → Settings tab, budget config
/settings/connectors       → Settings tab, MCP connectors
```

## Consequences

### Positive
- Logical grouping reduces cognitive load
- Settings becomes a proper section instead of a dump page
- Future features (workflows, connectors, pricing) have clear homes
- Bottom tabs provide consistent top-level navigation
- Each tab's sidebar can grow independently

### Negative
- Existing bookmarks/links to `/budget` or `/schedules` need redirects
- More complex sidebar state (tab-dependent content)
- Settings page needs to be split into sub-pages

### Migration
- Budget page moves from bottom tab to Settings > Budgets
- Schedules page moves from bottom tab to Tasks > Schedules
- Knowledge link moves from sidebar secondary nav to Agents tab sidebar
- Procedures link moves from sidebar secondary nav to Workflows tab

## Related ADRs
- ADR-015: SvelteKit Web UI (current frontend architecture)
- ADR-009: Task and SOP System (tasks and procedures data model)
- ADR-010: Budget Controls (budget configuration moving to settings)
