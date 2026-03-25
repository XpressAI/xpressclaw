# ADR-017: Agent-Published Apps

## Status
Proposed

## Context

Agents should be able to create and publish small web applications for the user. Today, when a user asks an agent for something recurring — "track my stock portfolio daily" or "make me a macro nutrient tracker" — the agent can only respond with text. There's no way to deliver a persistent, interactive experience.

The key insight: agents already run in Docker containers with full development capabilities. They can write HTML, JavaScript, and server-side code. What's missing is:
1. A way for the agent to **publish** the result as a named app
2. A **container** to host the app's server-side code
3. A **surface** in the xpressclaw UI to display and interact with the app
4. A **conversation** link back to the creating agent for iteration

## Decision

### App Lifecycle

```
User asks agent → Agent builds app → Agent publishes via MCP tool →
App appears in sidebar → User interacts via iframe → User chats with
agent to iterate on the app
```

### 1. Publishing (MCP Tool)

Agents publish apps via a new MCP tool:

```json
{
  "name": "publish_app",
  "description": "Publish a web app for the user",
  "parameters": {
    "name": "string (unique app identifier, e.g. 'stocks')",
    "title": "string (display name, e.g. 'Stock Portfolio')",
    "icon": "string (optional emoji or URL)",
    "source_dir": "string (path to app source in workspace)",
    "port": "integer (port the app server listens on, default 3000)",
    "start_command": "string (command to start the server, e.g. 'node server.js')",
    "description": "string (what this app does)"
  }
}
```

When called, xpressclaw:
1. Copies the source from the agent's workspace into a versioned app directory
2. Builds and starts a container for the app (lightweight, based on the app type)
3. Registers the app in the database with the creating agent's ID
4. The app appears in the sidebar under "Apps"

### 2. App Container

Each published app runs in its own container:
- **Image**: Lightweight base (node:alpine, python:slim, or static nginx)
- **Networking**: Assigned a port, proxied through xpressclaw at `/apps/{name}/`
- **Storage**: Persistent volume for app data (SQLite, files, etc.)
- **API access**: Can call xpressclaw APIs for agent data, memory, tasks
- **Lifecycle**: Started/stopped with the xpressclaw server

The app container is separate from the agent container — the agent creates the app, but the app runs independently. The agent can update the app by publishing a new version.

### 3. UI Surface

Apps are displayed in the xpressclaw frontend via iframe:

```
┌──────────────┬────────────────────────────────────────┐
│ APPS         │ ┌─ Stock Portfolio ──────────── 💬 ↗ ┐ │
│  Dashboard   │ │                                    │ │
│  Stocks  ●   │ │   (iframe: /apps/stocks/)          │ │
│  Nutrition   │ │                                    │ │
│              │ │   App content rendered here        │ │
│ CONVERSATIONS│ │                                    │ │
│  ...         │ │                                    │ │
│              │ └────────────────────────────────────┘ │
│ AGENTS       │                                        │
│  ...         │                                        │
└──────────────┴────────────────────────────────────────┘
```

**Header bar** for each app shows:
- App title and icon
- **💬 Chat** button (top right) — opens a conversation with the creating agent, pre-scoped to this app
- **↗ Pop out** button — opens the app in a standalone browser window

### 4. App Conversation

The chat button opens (or resumes) a conversation with the agent that created the app. The conversation has the app context injected:

```
System: You are modifying the app "Stock Portfolio" (source at /workspace/apps/stocks/).
The user may ask you to add features, fix bugs, or change the design.
After making changes, call publish_app to deploy the update.
```

This creates a natural iteration loop: use the app → notice something → chat with the agent → agent updates → app refreshes.

### 5. Data Model

```sql
CREATE TABLE apps (
    id TEXT PRIMARY KEY,           -- unique name (e.g. 'stocks')
    title TEXT NOT NULL,           -- display name
    icon TEXT,                     -- emoji or URL
    description TEXT,
    agent_id TEXT NOT NULL,        -- creating agent
    conversation_id TEXT,          -- linked conversation for iteration
    container_id TEXT,             -- running container
    port INTEGER DEFAULT 3000,    -- container port
    source_version INTEGER DEFAULT 1,
    status TEXT DEFAULT 'running', -- running, stopped, error
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (agent_id) REFERENCES agents(id)
);
```

### 6. App Proxy

Xpressclaw proxies app traffic through the server:

```
GET /apps/{name}/**  →  proxy to container port
```

This avoids CORS issues, provides authentication, and allows the iframe to load from the same origin.

### 7. Versioning

Each `publish_app` call increments `source_version`. The previous version's source is kept (up to a configurable limit). The agent can also call:

```json
{
  "name": "list_app_versions",
  "parameters": { "app_name": "stocks" }
}
```

```json
{
  "name": "rollback_app",
  "parameters": { "app_name": "stocks", "version": 2 }
}
```

### 8. Built-in Dashboard

The "Dashboard" app is a built-in app that ships with xpressclaw. It's not agent-created — it's the default overview page. It appears first in the Apps list and cannot be deleted. This ensures there's always a landing page even with no agents running.

## Examples

### Stock Portfolio Tracker
```
User: "I want a daily update on my stock portfolio. Track AAPL, NVDA, TSLA."

Agent:
1. Creates a Node.js app with stock API integration
2. Stores portfolio in a SQLite database
3. Builds a chart + table UI with daily email digest
4. Calls publish_app(name="stocks", title="Stock Portfolio", ...)

User sees "Stock Portfolio" appear in sidebar, clicks it, sees their dashboard.
Later: "Can you add a pie chart for sector allocation?"
Agent updates the code, calls publish_app again, iframe refreshes.
```

### Macro Nutrient Tracker
```
User: "Make me an app to track what I eat and show macro breakdowns."

Agent:
1. Creates a food logging app with a search API
2. Tracks meals with protein/carbs/fat
3. Shows daily/weekly charts
4. Calls publish_app(name="nutrition", title="Nutrition Tracker", ...)
```

## Consequences

### Positive
- Agents become genuinely useful for recurring tasks — not just chat
- Natural iteration loop via app conversation
- Apps persist independently of agent sessions
- Users get custom tools without leaving xpressclaw
- Versioning allows safe updates with rollback

### Negative
- Each app is another container (resource usage)
- App security: iframe sandboxing needed, apps shouldn't access other apps' data
- Agent must be capable enough to write working web apps
- App container management adds complexity

### Security Considerations
- Apps run in containers with no access to the host filesystem
- Iframe uses `sandbox` attribute to restrict capabilities
- App API access is scoped to the creating agent's permissions
- Network isolation: apps can only reach xpressclaw APIs, not arbitrary hosts

## Future Extensions
- **App marketplace**: Share apps between xpressclaw instances
- **App templates**: Pre-built app scaffolds agents can start from
- **Collaborative apps**: Multiple agents contributing to one app
- **Mobile apps**: Apps that work well on phone-sized screens
- **Webhooks**: Apps that respond to external events

## Related ADRs
- ADR-015: SvelteKit Web UI (frontend architecture)
- ADR-016: Navigation Restructure (Apps section in Agents tab)
- ADR-003: Container Isolation (container management patterns)
- ADR-005: MCP Tool System (publish_app as MCP tool)
