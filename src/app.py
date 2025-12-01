"""HTMX-based web dashboard for XpressAI."""

from pathlib import Path
from fastapi import FastAPI, Request, Depends
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates
from fastapi.staticfiles import StaticFiles

from xpressai.core.runtime import Runtime, get_runtime


def create_app(runtime: Runtime | None = None) -> FastAPI:
    """Create the FastAPI application."""
    app = FastAPI(title="XpressAI Dashboard")
    
    # Store runtime
    app.state.runtime = runtime or get_runtime()
    
    # Setup templates
    template_dir = Path(__file__).parent / "templates"
    if template_dir.exists():
        templates = Jinja2Templates(directory=str(template_dir))
    else:
        templates = None
    
    @app.get("/", response_class=HTMLResponse)
    async def dashboard(request: Request):
        """Main dashboard page."""
        if templates is None:
            return HTMLResponse(_minimal_dashboard())
        
        runtime = app.state.runtime
        agents = await runtime.list_agents()
        task_counts = await runtime.get_task_counts()
        budget = await runtime.get_budget_summary()
        
        return templates.TemplateResponse("dashboard.html", {
            "request": request,
            "agents": agents,
            "task_counts": task_counts,
            "budget": budget,
        })
    
    @app.get("/api/status")
    async def api_status():
        """API endpoint for status."""
        runtime = app.state.runtime
        agents = await runtime.list_agents()
        
        return {
            "agents": [
                {
                    "id": a.id,
                    "name": a.name,
                    "status": a.status,
                    "backend": a.backend,
                }
                for a in agents
            ]
        }
    
    @app.post("/api/agents/{agent_id}/start")
    async def start_agent(agent_id: str):
        """Start an agent."""
        runtime = app.state.runtime
        agent = await runtime.start_agent(agent_id)
        return {"status": agent.status}
    
    @app.post("/api/agents/{agent_id}/stop")
    async def stop_agent(agent_id: str):
        """Stop an agent."""
        runtime = app.state.runtime
        agent = await runtime.stop_agent(agent_id)
        return {"status": agent.status}
    
    return app


def _minimal_dashboard() -> str:
    """Minimal dashboard HTML when templates aren't available."""
    return """
    <!DOCTYPE html>
    <html>
    <head>
        <title>XpressAI Dashboard</title>
        <script src="https://cdn.tailwindcss.com"></script>
        <script src="https://unpkg.com/htmx.org@1.9.10"></script>
    </head>
    <body class="bg-gray-900 text-gray-100 min-h-screen p-8">
        <h1 class="text-3xl font-bold text-indigo-400 mb-8">⚡ XpressAI Dashboard</h1>
        
        <div class="bg-gray-800 rounded-lg p-6 mb-6">
            <h2 class="text-xl font-semibold mb-4">Status</h2>
            <div hx-get="/api/status" hx-trigger="load, every 5s" hx-swap="innerHTML">
                Loading...
            </div>
        </div>
        
        <p class="text-gray-400">
            For the full dashboard experience, install templates with: pip install xpressai[web]
        </p>
    </body>
    </html>
    """


def run_dashboard(host: str = "127.0.0.1", port: int = 7777) -> None:
    """Run the web dashboard."""
    import uvicorn
    import webbrowser
    import threading
    
    print(f"Starting dashboard at http://{host}:{port}")
    
    # Open browser after a short delay
    def open_browser():
        import time
        time.sleep(1)
        webbrowser.open(f"http://{host}:{port}")
    
    threading.Thread(target=open_browser, daemon=True).start()
    
    app = create_app()
    uvicorn.run(app, host=host, port=port, log_level="warning")
