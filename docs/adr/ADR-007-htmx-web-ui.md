# ADR-007: HTMX Web UI

## Status
Superseded by ADR-015 (SvelteKit Web UI)

## Context

XpressAI needs a web dashboard for:
- Agent status and monitoring
- Task board (kanban)
- Memory inspection
- Budget overview
- Log viewing
- Configuration editing

Options considered:
1. **React/Vue/Svelte SPA**: Rich interactivity, complex build, heavy client
2. **HTMX + Server-rendered**: Simple, fast, minimal JS, progressive enhancement
3. **Streamlit/Gradio**: Quick prototypes, limited customization
4. **No web UI**: CLI/TUI only

Given our philosophy of simplicity and the server-side nature of our data, **HTMX** with server-rendered templates is the best fit.

## Decision

We will build the web UI using:
- **FastAPI**: Web framework (async, modern, good DX)
- **HTMX**: Dynamic behavior without SPA complexity
- **Jinja2**: Server-side templates
- **Tailwind CSS**: Utility-first styling (via CDN)
- **Alpine.js** (optional): For small client-side state when needed

### Why HTMX?

1. **Server is the source of truth**: All our state (agents, tasks, memory) lives server-side
2. **Real-time updates via SSE**: HTMX supports Server-Sent Events naturally
3. **Progressive enhancement**: Works without JS, enhanced with it
4. **Tiny client footprint**: HTMX is ~14KB
5. **Python templates**: No JS build step, same language as backend

### Application Structure

```
src/xpressai/web/
├── __init__.py
├── app.py              # FastAPI application
├── routes/
│   ├── __init__.py
│   ├── dashboard.py    # Main dashboard
│   ├── agents.py       # Agent management
│   ├── tasks.py        # Task board
│   ├── memory.py       # Memory browser
│   ├── budget.py       # Budget tracking
│   └── api.py          # JSON API endpoints
├── templates/
│   ├── base.html       # Base template with HTMX
│   ├── components/     # Reusable components
│   │   ├── agent_card.html
│   │   ├── task_item.html
│   │   ├── memory_slot.html
│   │   └── budget_bar.html
│   ├── dashboard.html
│   ├── agents/
│   │   ├── list.html
│   │   ├── detail.html
│   │   └── logs.html
│   ├── tasks/
│   │   ├── board.html
│   │   └── item.html
│   └── memory/
│       ├── browser.html
│       └── detail.html
└── static/
    └── css/
        └── custom.css  # Minimal custom styles
```

### Base Template

```html
<!-- templates/base.html -->
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{% block title %}XpressAI{% endblock %}</title>
    
    <!-- Tailwind via CDN -->
    <script src="https://cdn.tailwindcss.com"></script>
    
    <!-- HTMX -->
    <script src="https://unpkg.com/htmx.org@1.9.10"></script>
    <script src="https://unpkg.com/htmx.org@1.9.10/dist/ext/sse.js"></script>
    
    <!-- Alpine.js for small client-side state -->
    <script defer src="https://unpkg.com/alpinejs@3.x.x/dist/cdn.min.js"></script>
    
    <style>
        [htmx-indicator] { opacity: 0; }
        .htmx-request [htmx-indicator] { opacity: 1; }
    </style>
</head>
<body class="bg-gray-900 text-gray-100 min-h-screen">
    <nav class="bg-gray-800 border-b border-gray-700 px-4 py-3">
        <div class="flex items-center justify-between max-w-7xl mx-auto">
            <a href="/" class="text-xl font-bold text-indigo-400">⚡ XpressAI</a>
            <div class="flex gap-4">
                <a href="/agents" class="hover:text-indigo-300">Agents</a>
                <a href="/tasks" class="hover:text-indigo-300">Tasks</a>
                <a href="/memory" class="hover:text-indigo-300">Memory</a>
                <a href="/budget" class="hover:text-indigo-300">Budget</a>
            </div>
        </div>
    </nav>
    
    <main class="max-w-7xl mx-auto px-4 py-6">
        {% block content %}{% endblock %}
    </main>
    
    {% block scripts %}{% endblock %}
</body>
</html>
```

### Dashboard with Real-time Updates

```html
<!-- templates/dashboard.html -->
{% extends "base.html" %}

{% block content %}
<div class="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6">
    
    <!-- Agent Status - Updates via SSE -->
    <div class="bg-gray-800 rounded-lg p-4"
         hx-ext="sse"
         sse-connect="/api/sse/agents"
         sse-swap="message">
        <h2 class="text-lg font-semibold mb-4">Agents</h2>
        <div id="agent-list">
            {% for agent in agents %}
                {% include "components/agent_card.html" %}
            {% endfor %}
        </div>
    </div>
    
    <!-- Task Summary -->
    <div class="bg-gray-800 rounded-lg p-4">
        <h2 class="text-lg font-semibold mb-4">Tasks</h2>
        <div class="space-y-2">
            <div class="flex justify-between">
                <span>Pending</span>
                <span class="text-yellow-400">{{ task_counts.pending }}</span>
            </div>
            <div class="flex justify-between">
                <span>In Progress</span>
                <span class="text-blue-400">{{ task_counts.in_progress }}</span>
            </div>
            <div class="flex justify-between">
                <span>Completed Today</span>
                <span class="text-green-400">{{ task_counts.completed_today }}</span>
            </div>
        </div>
        <a href="/tasks" 
           class="mt-4 block text-center text-indigo-400 hover:text-indigo-300">
            View Board →
        </a>
    </div>
    
    <!-- Budget Overview -->
    <div class="bg-gray-800 rounded-lg p-4"
         hx-get="/api/budget/summary"
         hx-trigger="every 30s">
        <h2 class="text-lg font-semibold mb-4">Budget</h2>
        {% include "components/budget_bar.html" %}
    </div>
    
</div>

<!-- Recent Activity Log - Streams via SSE -->
<div class="mt-6 bg-gray-800 rounded-lg p-4"
     hx-ext="sse"
     sse-connect="/api/sse/activity">
    <h2 class="text-lg font-semibold mb-4">Recent Activity</h2>
    <div id="activity-log" 
         class="space-y-2 max-h-96 overflow-y-auto font-mono text-sm"
         sse-swap="message"
         hx-swap="afterbegin">
        <!-- Activity items streamed here -->
    </div>
</div>
{% endblock %}
```

### Agent Card Component

```html
<!-- templates/components/agent_card.html -->
<div class="flex items-center justify-between p-3 bg-gray-700 rounded-lg"
     id="agent-{{ agent.id }}">
    <div class="flex items-center gap-3">
        <!-- Status indicator -->
        <div class="w-3 h-3 rounded-full 
            {% if agent.status == 'running' %}bg-green-400 animate-pulse
            {% elif agent.status == 'error' %}bg-red-400
            {% else %}bg-gray-400{% endif %}">
        </div>
        <div>
            <div class="font-medium">{{ agent.name }}</div>
            <div class="text-sm text-gray-400">{{ agent.backend }}</div>
        </div>
    </div>
    
    <div class="flex gap-2">
        {% if agent.status == 'running' %}
        <button hx-post="/api/agents/{{ agent.id }}/stop"
                hx-swap="outerHTML"
                hx-target="#agent-{{ agent.id }}"
                class="px-3 py-1 bg-red-600 hover:bg-red-500 rounded text-sm">
            Stop
        </button>
        {% else %}
        <button hx-post="/api/agents/{{ agent.id }}/start"
                hx-swap="outerHTML"
                hx-target="#agent-{{ agent.id }}"
                class="px-3 py-1 bg-green-600 hover:bg-green-500 rounded text-sm">
            Start
        </button>
        {% endif %}
        
        <a href="/agents/{{ agent.id }}" 
           class="px-3 py-1 bg-gray-600 hover:bg-gray-500 rounded text-sm">
            Details
        </a>
    </div>
</div>
```

### Task Board (Kanban)

```html
<!-- templates/tasks/board.html -->
{% extends "base.html" %}

{% block content %}
<div class="flex gap-4 overflow-x-auto pb-4">
    {% for column in ['pending', 'in_progress', 'completed'] %}
    <div class="flex-shrink-0 w-80 bg-gray-800 rounded-lg p-4">
        <h3 class="font-semibold mb-4 capitalize">
            {{ column | replace('_', ' ') }}
            <span class="text-gray-400">({{ tasks[column] | length }})</span>
        </h3>
        
        <div class="space-y-2"
             id="column-{{ column }}"
             hx-post="/api/tasks/reorder"
             hx-trigger="dragend"
             hx-vals='{"column": "{{ column }}"}'>
            
            {% for task in tasks[column] %}
            <div class="bg-gray-700 rounded p-3 cursor-move"
                 draggable="true"
                 id="task-{{ task.id }}">
                <div class="font-medium">{{ task.title }}</div>
                {% if task.description %}
                <div class="text-sm text-gray-400 mt-1">
                    {{ task.description | truncate(100) }}
                </div>
                {% endif %}
                <div class="flex justify-between mt-2 text-xs text-gray-500">
                    <span>{{ task.agent_id or 'Unassigned' }}</span>
                    <span>{{ task.created_at | timeago }}</span>
                </div>
            </div>
            {% endfor %}
            
        </div>
        
        <!-- Quick add -->
        <form hx-post="/api/tasks"
              hx-target="#column-{{ column }}"
              hx-swap="beforeend"
              class="mt-4">
            <input type="hidden" name="status" value="{{ column }}">
            <input type="text" 
                   name="title"
                   placeholder="+ Add task"
                   class="w-full bg-gray-900 rounded px-3 py-2 text-sm">
        </form>
    </div>
    {% endfor %}
</div>
{% endblock %}
```

### FastAPI Routes

```python
# routes/dashboard.py
from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates = Jinja2Templates(directory="templates")

@router.get("/", response_class=HTMLResponse)
async def dashboard(request: Request, runtime: Runtime = Depends(get_runtime)):
    agents = await runtime.list_agents()
    task_counts = await runtime.get_task_counts()
    budget = await runtime.get_budget_summary()
    
    return templates.TemplateResponse("dashboard.html", {
        "request": request,
        "agents": agents,
        "task_counts": task_counts,
        "budget": budget,
    })

# SSE endpoint for real-time updates
@router.get("/api/sse/agents")
async def agent_sse(runtime: Runtime = Depends(get_runtime)):
    async def event_generator():
        async for event in runtime.agent_events():
            # Re-render agent card component
            html = templates.get_template("components/agent_card.html").render(
                agent=event.agent
            )
            yield {
                "event": "message",
                "data": html
            }
    
    return EventSourceResponse(event_generator())

@router.get("/api/sse/activity")
async def activity_sse(runtime: Runtime = Depends(get_runtime)):
    async def event_generator():
        async for log in runtime.activity_stream():
            html = f"""
            <div class="text-gray-300">
                <span class="text-gray-500">{log.timestamp}</span>
                <span class="text-indigo-400">[{log.agent}]</span>
                {log.message}
            </div>
            """
            yield {"event": "message", "data": html}
    
    return EventSourceResponse(event_generator())
```

### API for HTMX Actions

```python
# routes/api.py
from fastapi import APIRouter, Form

router = APIRouter(prefix="/api")

@router.post("/agents/{agent_id}/start")
async def start_agent(
    agent_id: str, 
    runtime: Runtime = Depends(get_runtime)
):
    agent = await runtime.start_agent(agent_id)
    return templates.TemplateResponse("components/agent_card.html", {
        "request": request,
        "agent": agent
    })

@router.post("/agents/{agent_id}/stop")
async def stop_agent(
    agent_id: str,
    runtime: Runtime = Depends(get_runtime)
):
    agent = await runtime.stop_agent(agent_id)
    return templates.TemplateResponse("components/agent_card.html", {
        "request": request,
        "agent": agent
    })

@router.post("/tasks")
async def create_task(
    title: str = Form(...),
    status: str = Form("pending"),
    runtime: Runtime = Depends(get_runtime)
):
    task = await runtime.create_task(title=title, status=status)
    return templates.TemplateResponse("tasks/item.html", {
        "request": request,
        "task": task
    })
```

### Running the Web UI

```python
# app.py
from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles
import uvicorn

def create_app(runtime: Runtime) -> FastAPI:
    app = FastAPI(title="XpressAI Dashboard")
    
    # Mount static files
    app.mount("/static", StaticFiles(directory="static"), name="static")
    
    # Include routers
    app.include_router(dashboard.router)
    app.include_router(agents.router)
    app.include_router(tasks.router)
    app.include_router(memory.router)
    app.include_router(api.router)
    
    # Dependency injection for runtime
    app.state.runtime = runtime
    
    return app

def run_dashboard(runtime: Runtime, port: int = 7777):
    app = create_app(runtime)
    uvicorn.run(app, host="127.0.0.1", port=port)
```

## Consequences

### Positive
- Simple mental model: server renders HTML, HTMX adds interactivity
- No JS build step or node_modules
- Fast initial page loads
- SSE provides real-time updates without WebSocket complexity
- Easy to test (just HTML responses)
- Works without JavaScript (progressive enhancement)

### Negative
- Less client-side interactivity than SPA
- Some features require page-level refreshes
- Tailwind CDN increases page size (can purge for production)
- HTMX patterns less familiar to frontend developers

### Implementation Notes

1. Start with dashboard and agent list
2. Add SSE for real-time agent status
3. Build task board with drag-and-drop
4. Add memory browser with search
5. Consider adding dark/light theme toggle

## Related ADRs
- ADR-008: Textual TUI (alternative interface)
- ADR-009: Task System (displayed in board)
