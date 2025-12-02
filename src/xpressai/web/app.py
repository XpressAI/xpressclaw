"""FastAPI + HTMX Web Dashboard for XpressAI.

Server-rendered web UI with minimal JavaScript, using HTMX for interactivity.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.core.runtime import Runtime

try:
    from fastapi import FastAPI, Request, HTTPException
    from fastapi.responses import HTMLResponse
    from fastapi.templating import Jinja2Templates
    from fastapi.staticfiles import StaticFiles

    FASTAPI_AVAILABLE = True
except ImportError:
    FASTAPI_AVAILABLE = False
    FastAPI = None  # type: ignore


# Global runtime reference (set when app is created)
_runtime: Runtime | None = None


def create_app(runtime: Runtime | None = None) -> FastAPI:
    """Create the FastAPI application.

    Args:
        runtime: Optional runtime instance to use

    Returns:
        Configured FastAPI application
    """
    global _runtime
    _runtime = runtime

    if not FASTAPI_AVAILABLE:
        raise ImportError(
            "FastAPI is not installed. Install with: pip install 'xpressai[web]' "
            "or pip install fastapi uvicorn jinja2"
        )

    app = FastAPI(
        title="XpressAI Dashboard",
        description="Web dashboard for XpressAI agent runtime",
        version="0.1.0",
    )

    # Templates directory
    template_dir = Path(__file__).parent / "templates"
    if template_dir.exists():
        templates = Jinja2Templates(directory=str(template_dir))
    else:
        templates = None

    @app.get("/", response_class=HTMLResponse)
    async def index(request: Request):
        """Dashboard home page."""
        if templates:
            return templates.TemplateResponse(
                "index.html", {"request": request, "title": "XpressAI Dashboard"}
            )

        # Inline template when templates directory doesn't exist
        return HTMLResponse(content=_get_inline_index())

    @app.get("/api/status")
    async def get_status():
        """Get current system status."""
        if not _runtime:
            return {"status": "no_runtime", "agents": [], "budget": {}}

        agents = await _runtime.list_agents()
        budget = await _runtime.get_budget_summary()

        return {
            "status": "running" if _runtime.is_running else "stopped",
            "agents": [{"name": a.name, "status": a.status, "backend": a.backend} for a in agents],
            "budget": budget,
        }

    @app.get("/api/agents")
    async def list_agents():
        """List all agents."""
        if not _runtime:
            return {"agents": []}

        agents = await _runtime.list_agents()
        return {
            "agents": [{"name": a.name, "status": a.status, "backend": a.backend} for a in agents]
        }

    @app.get("/api/agents/{agent_name}")
    async def get_agent(agent_name: str):
        """Get details for a specific agent."""
        if not _runtime:
            raise HTTPException(status_code=503, detail="Runtime not available")

        agent = await _runtime.get_agent(agent_name)
        if not agent:
            raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")

        return {"name": agent.name, "status": agent.status, "backend": agent.backend}

    @app.get("/api/budget")
    async def get_budget():
        """Get budget status."""
        if not _runtime:
            return {"error": "Runtime not available"}

        return await _runtime.get_budget_summary()

    @app.get("/api/tasks")
    async def list_tasks():
        """List all tasks."""
        if not _runtime:
            return {"tasks": []}

        counts = await _runtime.get_task_counts()
        return {"counts": counts}

    # HTMX partials
    @app.get("/partials/agents", response_class=HTMLResponse)
    async def agents_partial(request: Request):
        """HTMX partial for agents list."""
        if not _runtime:
            return HTMLResponse("<p>No runtime available</p>")

        agents = await _runtime.list_agents()

        html_parts = ["<div class='agent-list'>"]
        for agent in agents:
            status_class = {
                "running": "status-running",
                "stopped": "status-stopped",
                "error": "status-error",
            }.get(agent.status, "status-unknown")

            html_parts.append(f"""
                <div class="agent-card">
                    <span class="agent-status {status_class}">{agent.status}</span>
                    <span class="agent-name">{agent.name}</span>
                </div>
            """)

        html_parts.append("</div>")
        return HTMLResponse("".join(html_parts))

    @app.get("/partials/budget", response_class=HTMLResponse)
    async def budget_partial(request: Request):
        """HTMX partial for budget display."""
        if not _runtime:
            return HTMLResponse("<p>Budget tracking not available</p>")

        summary = await _runtime.get_budget_summary()
        total_spent = float(summary.get("total_spent", 0))
        limit = summary.get("limit")

        if limit:
            limit = float(limit)
            pct = (total_spent / limit * 100) if limit > 0 else 0
            return HTMLResponse(f"""
                <div class="budget-display">
                    <div class="budget-bar" style="--progress: {pct}%">
                        <div class="budget-fill"></div>
                    </div>
                    <p>${total_spent:.2f} / ${limit:.2f} ({pct:.1f}%)</p>
                </div>
            """)
        else:
            return HTMLResponse(f"""
                <div class="budget-display">
                    <p>Spent: ${total_spent:.2f} (no limit set)</p>
                </div>
            """)

    return app


def _get_inline_index() -> str:
    """Get inline HTML template for when templates dir doesn't exist."""
    return """
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>XpressAI Dashboard</title>
    <script src="https://unpkg.com/htmx.org@1.9.10"></script>
    <style>
        :root {
            --bg: #0d1117;
            --fg: #c9d1d9;
            --accent: #58a6ff;
            --success: #3fb950;
            --warning: #d29922;
            --error: #f85149;
        }
        * { box-sizing: border-box; margin: 0; padding: 0; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: var(--bg);
            color: var(--fg);
            line-height: 1.6;
            padding: 2rem;
        }
        h1 { color: var(--accent); margin-bottom: 2rem; }
        .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr)); gap: 1.5rem; }
        .card {
            background: #161b22;
            border: 1px solid #30363d;
            border-radius: 8px;
            padding: 1.5rem;
        }
        .card h2 { color: var(--accent); margin-bottom: 1rem; font-size: 1.1rem; }
        .status-running { color: var(--success); }
        .status-stopped { color: var(--error); }
        .status-error { color: var(--error); }
        .agent-card { padding: 0.5rem 0; border-bottom: 1px solid #30363d; }
        .agent-card:last-child { border-bottom: none; }
        .budget-bar {
            height: 8px;
            background: #30363d;
            border-radius: 4px;
            overflow: hidden;
            margin-bottom: 0.5rem;
        }
        .budget-fill {
            height: 100%;
            width: var(--progress, 0%);
            background: var(--accent);
            transition: width 0.3s;
        }
    </style>
</head>
<body>
    <h1>XpressAI Dashboard</h1>
    
    <div class="grid">
        <div class="card">
            <h2>Agents</h2>
            <div hx-get="/partials/agents" hx-trigger="load, every 5s" hx-swap="innerHTML">
                Loading...
            </div>
        </div>
        
        <div class="card">
            <h2>Budget</h2>
            <div hx-get="/partials/budget" hx-trigger="load, every 10s" hx-swap="innerHTML">
                Loading...
            </div>
        </div>
    </div>
</body>
</html>
"""


def run_web(runtime: Runtime | None = None, host: str = "127.0.0.1", port: int = 8080) -> None:
    """Run the web dashboard.

    Args:
        runtime: Optional runtime instance to monitor
        host: Host to bind to
        port: Port to listen on
    """
    if not FASTAPI_AVAILABLE:
        print("Error: FastAPI is not installed.")
        print("Install with: pip install 'xpressai[web]' or pip install fastapi uvicorn jinja2")
        return

    try:
        import uvicorn
    except ImportError:
        print("Error: uvicorn is not installed.")
        print("Install with: pip install uvicorn")
        return

    app = create_app(runtime)
    uvicorn.run(app, host=host, port=port)
