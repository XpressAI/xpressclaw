"""FastAPI + HTMX Web Dashboard for XpressAI.

Server-rendered web UI with minimal JavaScript, using HTMX for interactivity.
"""

from __future__ import annotations

import asyncio
import logging
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING, Optional

logger = logging.getLogger(__name__)

if TYPE_CHECKING:
    from xpressai.core.runtime import Runtime

try:
    from fastapi import FastAPI, Request, HTTPException, Form
    from fastapi.responses import HTMLResponse, RedirectResponse
    from fastapi.templating import Jinja2Templates
    from fastapi.staticfiles import StaticFiles
    from pydantic import BaseModel

    FASTAPI_AVAILABLE = True
except ImportError:
    FASTAPI_AVAILABLE = False
    FastAPI = None  # type: ignore
    BaseModel = object  # type: ignore


class CreateTaskRequest(BaseModel):
    """Request body for creating a task."""
    title: str
    description: Optional[str] = None
    agent_id: Optional[str] = None


class AddMessageRequest(BaseModel):
    """Request body for adding a message to a task."""
    content: str


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

    # Static files and templates
    web_dir = Path(__file__).parent
    static_dir = web_dir / "static"
    template_dir = web_dir / "templates"

    if static_dir.exists():
        app.mount("/static", StaticFiles(directory=str(static_dir)), name="static")

    templates = None
    if template_dir.exists() and (template_dir / "index.html").exists():
        templates = Jinja2Templates(directory=str(template_dir))

    # -------------------------
    # Page Routes
    # -------------------------

    @app.get("/", response_class=HTMLResponse)
    async def index(request: Request):
        """Dashboard home page."""
        if templates:
            return templates.TemplateResponse(
                "index.html", {"request": request, "active": "dashboard"}
            )
        return HTMLResponse(content=_get_inline_index())

    @app.get("/agents", response_class=HTMLResponse)
    async def agents_page(request: Request):
        """Agents list page."""
        from pathlib import Path
        import yaml

        agents = []
        agent_configs = {}
        agent_budgets = {}

        if _runtime:
            agents = await _runtime.list_agents()

            # Load agent configs from yaml for model/backend info
            config_path = Path.cwd() / "xpressai.yaml"
            if config_path.exists():
                with open(config_path) as f:
                    config_data = yaml.safe_load(f) or {}
                for agent_cfg in config_data.get("agents", []):
                    agent_configs[agent_cfg.get("name")] = agent_cfg

            # Get budget info for each agent
            for agent in agents:
                budget = await _runtime.get_budget_summary(agent.name)
                agent_budgets[agent.name] = budget

        if templates:
            return templates.TemplateResponse(
                "agents.html",
                {"request": request, "agents": agents, "agent_configs": agent_configs, "agent_budgets": agent_budgets, "active": "agents"},
            )
        return HTMLResponse("<h1>Agents - Templates not installed</h1>")

    @app.get("/agents/new", response_class=HTMLResponse)
    async def new_agent_page(request: Request):
        """New agent creation page."""
        if templates:
            return templates.TemplateResponse(
                "agent_new.html",
                {"request": request, "active": "agents"},
            )
        return HTMLResponse("<h1>New Agent - Templates not installed</h1>")

    @app.get("/agent/{agent_id}/edit", response_class=HTMLResponse)
    async def agent_edit_page(request: Request, agent_id: str):
        """Agent edit page."""
        from pathlib import Path
        import yaml

        agent = None
        agent_config = None
        if _runtime:
            agent = await _runtime.get_agent(agent_id)

            # Load agent config from yaml
            config_path = Path.cwd() / "xpressai.yaml"
            if config_path.exists():
                with open(config_path) as f:
                    config_data = yaml.safe_load(f) or {}
                for ac in config_data.get("agents", []):
                    if ac.get("name") == agent_id:
                        agent_config = ac
                        break

        if not agent:
            raise HTTPException(status_code=404, detail=f"Agent not found: {agent_id}")

        # Don't allow editing running agents
        if agent.status == "running":
            raise HTTPException(status_code=400, detail="Stop agent before editing")

        if templates:
            return templates.TemplateResponse(
                "agent_edit.html",
                {"request": request, "agent": agent, "agent_config": agent_config, "active": "agents"},
            )
        return HTMLResponse("<h1>Edit Agent - Templates not installed</h1>")

    @app.get("/agent/{agent_id}/chat", response_class=HTMLResponse)
    async def agent_chat_page(request: Request, agent_id: str):
        """Agent chat page."""
        agent = None
        if _runtime:
            agent = await _runtime.get_agent(agent_id)

        if not agent:
            raise HTTPException(status_code=404, detail=f"Agent not found: {agent_id}")

        if templates:
            return templates.TemplateResponse(
                "agent_chat.html",
                {"request": request, "agent": agent, "active": "agents"},
            )
        return HTMLResponse("<h1>Agent Chat - Templates not installed</h1>")

    @app.get("/tasks", response_class=HTMLResponse)
    async def tasks_page(request: Request):
        """Tasks page."""
        agents = []
        if _runtime:
            agents = await _runtime.list_agents()
            print(f"[DEBUG] Tasks page: found {len(agents)} agents: {[a.name for a in agents]}")
        else:
            print("[DEBUG] Tasks page: _runtime is None")
        if templates:
            return templates.TemplateResponse(
                "tasks.html", {"request": request, "active": "tasks", "agents": agents}
            )
        return HTMLResponse("<h1>Tasks - Templates not installed</h1>")

    @app.get("/memory", response_class=HTMLResponse)
    async def memory_page(request: Request):
        """Memory page (zettelkasten browser)."""
        agents = []
        if _runtime:
            agents = await _runtime.list_agents()
        if templates:
            return templates.TemplateResponse(
                "zettelkasten.html", {"request": request, "active": "memory", "agents": agents}
            )
        return HTMLResponse("<h1>Memory - Templates not installed</h1>")

    @app.get("/zettelkasten", response_class=HTMLResponse)
    async def zettelkasten_page(request: Request):
        """Zettelkasten browser page."""
        agents = []
        if _runtime:
            agents = await _runtime.list_agents()
        if templates:
            return templates.TemplateResponse(
                "zettelkasten.html", {"request": request, "active": "zettelkasten", "agents": agents}
            )
        return HTMLResponse("<h1>Zettelkasten - Templates not installed</h1>")

    @app.get("/logs", response_class=HTMLResponse)
    async def logs_page(request: Request):
        """Logs page."""
        agents = []
        if _runtime:
            agents = await _runtime.list_agents()
        if templates:
            return templates.TemplateResponse(
                "logs.html", {"request": request, "active": "logs", "agents": agents}
            )
        return HTMLResponse("<h1>Logs - Templates not installed</h1>")

    @app.get("/procedures", response_class=HTMLResponse)
    async def procedures_page(request: Request):
        """Procedures (SOP) page."""
        agents = []
        if _runtime:
            agents = await _runtime.list_agents()
        if templates:
            return templates.TemplateResponse(
                "procedures.html", {"request": request, "active": "procedures", "agents": agents}
            )
        return HTMLResponse("<h1>Procedures - Templates not installed</h1>")

    @app.get("/task/{task_id}", response_class=HTMLResponse)
    async def task_detail_page(request: Request, task_id: str):
        """Task detail page with conversation thread."""
        if not _runtime or not _runtime.task_board:
            return HTMLResponse("<h1>Runtime not available</h1>")

        try:
            task = await _runtime.task_board.get_task(task_id)
        except Exception:
            raise HTTPException(status_code=404, detail="Task not found")

        # Get conversation messages
        messages = []
        if hasattr(_runtime, 'conversation_manager') and _runtime.conversation_manager:
            messages = await _runtime.conversation_manager.get_messages(task_id)

        # Get agents for assignment dropdown
        agents = await _runtime.list_agents()

        if templates:
            return templates.TemplateResponse(
                "task_detail.html",
                {
                    "request": request,
                    "active": "tasks",
                    "task": task,
                    "messages": messages,
                    "agents": agents,
                }
            )
        return HTMLResponse(f"<h1>Task: {task.title}</h1><p>{task.description}</p>")

    # -------------------------
    # API Routes
    # -------------------------

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

    @app.get("/api/health")
    async def health_check():
        """Health check endpoint."""
        status = "connected" if _runtime and _runtime.is_initialized else "disconnected"
        return {"status": status}

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

    @app.post("/api/agents/{agent_name}/start")
    async def start_agent(agent_name: str):
        """Start a specific agent."""
        if not _runtime:
            raise HTTPException(status_code=503, detail="Runtime not available")

        try:
            agent = await _runtime.start_agent(agent_name)
            return {"success": True, "name": agent.name, "status": agent.status}
        except Exception as e:
            error_msg = str(e)
            if "not found" in error_msg.lower() or "unknown agent" in error_msg.lower():
                raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")
            if "already running" in error_msg.lower():
                raise HTTPException(status_code=400, detail=f"Agent '{agent_name}' is already running")
            raise HTTPException(status_code=500, detail=error_msg)

    @app.post("/api/agents/{agent_name}/stop")
    async def stop_agent(agent_name: str):
        """Stop a specific agent."""
        if not _runtime:
            raise HTTPException(status_code=503, detail="Runtime not available")

        try:
            agent = await _runtime.stop_agent(agent_name)
            return {"success": True, "name": agent.name, "status": agent.status}
        except Exception as e:
            error_msg = str(e)
            if "not found" in error_msg.lower() or "unknown agent" in error_msg.lower():
                raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")
            raise HTTPException(status_code=500, detail=error_msg)

    @app.post("/api/agents")
    async def create_agent(request: Request):
        """Create a new agent and save to xpressai.yaml."""
        from pathlib import Path
        import yaml
        import re

        try:
            data = await request.json()
        except Exception:
            raise HTTPException(status_code=400, detail="Invalid JSON body")

        # Validate required fields
        name = data.get("name", "").strip().lower()
        if not name:
            raise HTTPException(status_code=400, detail="Agent name is required")

        # Validate name format
        if not re.match(r'^[a-z][a-z0-9_-]*$', name):
            raise HTTPException(
                status_code=400,
                detail="Agent name must start with a letter and contain only lowercase letters, numbers, hyphens, and underscores"
            )

        backend = data.get("backend", "local")
        role = data.get("role", "You are a helpful AI assistant.")
        tools = data.get("tools", [])
        hooks = data.get("hooks", {"before_message": [], "after_message": []})

        # Load existing config
        config_path = Path.cwd() / "xpressai.yaml"

        if config_path.exists():
            with open(config_path) as f:
                config_data = yaml.safe_load(f) or {}
        else:
            config_data = {
                "system": {
                    "isolation": "docker",
                    "budget": {"daily": "$20.00", "on_exceeded": "pause"}
                },
                "agents": [],
                "tools": {"builtin": {"filesystem": True, "shell": {"enabled": True}}},
                "memory": {"near_term_slots": 8, "eviction": "least-recently-relevant"},
            }

        # Check if agent already exists
        existing_agents = config_data.get("agents", [])
        for agent in existing_agents:
            if agent.get("name") == name:
                raise HTTPException(status_code=400, detail=f"Agent '{name}' already exists")

        # Build agent config
        new_agent = {
            "name": name,
            "backend": backend,
            "role": role,
        }

        # Add model for Claude/OpenAI backends
        model = data.get("model")
        if model and backend in ("claude", "openai"):
            new_agent["model"] = model

        if tools:
            new_agent["tools"] = tools

        # Add hooks if specified
        if hooks.get("before_message") or hooks.get("after_message"):
            new_agent["hooks"] = {}
            if hooks.get("before_message"):
                new_agent["hooks"]["before_message"] = hooks["before_message"]
            if hooks.get("after_message"):
                new_agent["hooks"]["after_message"] = hooks["after_message"]

        # Add local_model config if backend is local
        local_model = data.get("local_model")
        if backend == "local" and local_model:
            new_agent["local_model"] = {
                "model": local_model.get("model", "Qwen/Qwen3-8B"),
                "inference_backend": local_model.get("inference_backend", "vllm"),
                "base_url": local_model.get("base_url", "http://localhost:8000"),
                "context_length": local_model.get("context_length", 32768),
                "tool_format": local_model.get("tool_format", "xml"),
                "thinking_mode": local_model.get("thinking_mode", "auto"),
                "max_tool_calls": local_model.get("max_tool_calls", 20),
            }
            # Only include api_key if it's not empty/EMPTY
            api_key = local_model.get("api_key", "")
            if api_key and api_key != "EMPTY":
                new_agent["local_model"]["api_key"] = api_key
            # Add pricing if specified
            price_input = local_model.get("price_input")
            price_output = local_model.get("price_output")
            if price_input is not None:
                new_agent["local_model"]["price_input"] = price_input
            if price_output is not None:
                new_agent["local_model"]["price_output"] = price_output

        # Add budget config if specified
        budget = data.get("budget")
        if budget:
            new_agent["budget"] = {}
            if budget.get("daily"):
                new_agent["budget"]["daily"] = f"${budget['daily']:.2f}"
            if budget.get("monthly"):
                new_agent["budget"]["monthly"] = f"${budget['monthly']:.2f}"
            if budget.get("on_exceeded"):
                new_agent["budget"]["on_exceeded"] = budget["on_exceeded"]

        # Add to agents list
        if "agents" not in config_data:
            config_data["agents"] = []
        config_data["agents"].append(new_agent)

        # Write back to YAML
        with open(config_path, "w") as f:
            yaml.dump(config_data, f, default_flow_style=False, sort_keys=False, allow_unicode=True)

        # Reload config to register the new agent in the runtime
        if _runtime:
            _runtime.reload_config()

        return {"success": True, "agent": {"name": name, "backend": backend, "status": "stopped"}}

    @app.put("/api/agents/{agent_name}")
    async def update_agent(agent_name: str, request: Request):
        """Update an existing agent's configuration."""
        from pathlib import Path
        import yaml

        # Check if agent is running
        if _runtime:
            agent = await _runtime.get_agent(agent_name)
            if agent and agent.status == "running":
                raise HTTPException(status_code=400, detail="Stop agent before editing")

        try:
            data = await request.json()
        except Exception:
            raise HTTPException(status_code=400, detail="Invalid JSON body")

        # Load existing config
        config_path = Path.cwd() / "xpressai.yaml"
        if not config_path.exists():
            raise HTTPException(status_code=404, detail="No xpressai.yaml found")

        with open(config_path) as f:
            config_data = yaml.safe_load(f) or {}

        # Find and update the agent
        agents = config_data.get("agents", [])
        agent_found = False
        for i, agent in enumerate(agents):
            if agent.get("name") == agent_name:
                agent_found = True
                # Update fields
                if "role" in data:
                    agents[i]["role"] = data["role"]
                if "model" in data:
                    agents[i]["model"] = data["model"]
                if "tools" in data:
                    agents[i]["tools"] = data["tools"]
                if "hooks" in data:
                    agents[i]["hooks"] = data["hooks"]
                if "local_model" in data:
                    local_model = data["local_model"]
                    agents[i]["local_model"] = {
                        "model": local_model.get("model", "qwen3:8b"),
                        "inference_backend": local_model.get("inference_backend", "ollama"),
                        "base_url": local_model.get("base_url", "http://localhost:11434"),
                        "context_length": local_model.get("context_length", 32768),
                        "tool_format": local_model.get("tool_format", "native"),
                        "thinking_mode": local_model.get("thinking_mode", "auto"),
                        "max_tool_calls": local_model.get("max_tool_calls", 20),
                    }
                    api_key = local_model.get("api_key", "")
                    if api_key and api_key != "EMPTY":
                        agents[i]["local_model"]["api_key"] = api_key
                    # Add pricing if specified
                    price_input = local_model.get("price_input")
                    price_output = local_model.get("price_output")
                    if price_input is not None:
                        agents[i]["local_model"]["price_input"] = price_input
                    if price_output is not None:
                        agents[i]["local_model"]["price_output"] = price_output
                # Handle budget config
                if "budget" in data:
                    budget = data["budget"]
                    agents[i]["budget"] = {}
                    if budget.get("daily"):
                        agents[i]["budget"]["daily"] = f"${budget['daily']:.2f}"
                    if budget.get("monthly"):
                        agents[i]["budget"]["monthly"] = f"${budget['monthly']:.2f}"
                    if budget.get("on_exceeded"):
                        agents[i]["budget"]["on_exceeded"] = budget["on_exceeded"]
                elif "budget" in agents[i]:
                    # Remove budget if not provided (cleared)
                    del agents[i]["budget"]
                break

        if not agent_found:
            raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")

        config_data["agents"] = agents

        # Write back to YAML
        with open(config_path, "w") as f:
            yaml.dump(config_data, f, default_flow_style=False, sort_keys=False, allow_unicode=True)

        # Reload config
        if _runtime:
            _runtime.reload_config()

        return {"success": True, "agent": {"name": agent_name}}

    @app.delete("/api/agents/{agent_name}")
    async def delete_agent(agent_name: str):
        """Delete an agent from the configuration."""
        from pathlib import Path
        import yaml

        # Check if agent is running
        if _runtime:
            agent = await _runtime.get_agent(agent_name)
            if agent and agent.status == "running":
                raise HTTPException(status_code=400, detail="Stop agent before deleting")

        # Load existing config
        config_path = Path.cwd() / "xpressai.yaml"
        if not config_path.exists():
            raise HTTPException(status_code=404, detail="No xpressai.yaml found")

        with open(config_path) as f:
            config_data = yaml.safe_load(f) or {}

        # Find and remove the agent
        agents = config_data.get("agents", [])
        original_count = len(agents)
        agents = [a for a in agents if a.get("name") != agent_name]

        if len(agents) == original_count:
            raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")

        config_data["agents"] = agents

        # Write back to YAML
        with open(config_path, "w") as f:
            yaml.dump(config_data, f, default_flow_style=False, sort_keys=False, allow_unicode=True)

        # Reload config to remove from runtime
        if _runtime:
            _runtime.reload_config()

        return {"success": True, "message": f"Agent '{agent_name}' deleted"}

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

    @app.post("/api/tasks")
    async def create_task(
        title: str = Form(...),
        description: Optional[str] = Form(None),
        agent_id: Optional[str] = Form(None),
    ):
        """Create a new task from form data."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        # Convert empty string to None for agent_id
        if agent_id == "":
            agent_id = None

        task = await _runtime.task_board.create_task(
            title=title,
            description=description if description else None,
            agent_id=agent_id,
        )

        return {
            "id": task.id,
            "title": task.title,
            "status": task.status.value,
        }

    @app.get("/api/tasks/{task_id}")
    async def get_task(task_id: str):
        """Get a specific task."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        try:
            task = await _runtime.task_board.get_task(task_id)
        except Exception:
            raise HTTPException(status_code=404, detail="Task not found")

        return {
            "id": task.id,
            "title": task.title,
            "description": task.description,
            "status": task.status.value,
            "agent_id": task.agent_id,
            "created_at": task.created_at.isoformat(),
        }

    @app.get("/api/tasks/{task_id}/messages")
    async def get_task_messages(task_id: str):
        """Get messages for a task."""
        if not _runtime:
            raise HTTPException(status_code=503, detail="Runtime not available")

        if not hasattr(_runtime, 'conversation_manager') or not _runtime.conversation_manager:
            return {"messages": []}

        messages = await _runtime.conversation_manager.get_messages(task_id)
        return {
            "messages": [
                {
                    "id": m.id,
                    "role": m.role,
                    "content": m.content,
                    "timestamp": m.timestamp.isoformat(),
                }
                for m in messages
            ]
        }

    @app.post("/api/tasks/{task_id}/messages")
    async def add_task_message(task_id: str, content: str = Form(...)):
        """Add a user message to a task.

        If the task is waiting, completed, or blocked, this will resume it.
        """
        if not _runtime:
            raise HTTPException(status_code=503, detail="Runtime not available")

        if not hasattr(_runtime, 'conversation_manager') or not _runtime.conversation_manager:
            raise HTTPException(status_code=503, detail="Conversation manager not available")

        # Get current task status
        task = await _runtime.task_board.get_task(task_id)

        from xpressai.tasks.board import TaskStatus

        # Add the user message
        await _runtime.conversation_manager.add_message(task_id, "user", content)

        # Resume task if it's in a terminal or waiting state
        if task.status in (TaskStatus.WAITING_FOR_INPUT, TaskStatus.COMPLETED, TaskStatus.BLOCKED):
            await _runtime.task_board.update_status(task_id, TaskStatus.PENDING)

        return {"status": "ok"}

    @app.post("/api/tasks/{task_id}/complete")
    async def complete_task_manual(task_id: str):
        """Manually mark a task as completed."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        from xpressai.tasks.board import TaskStatus

        try:
            task = await _runtime.task_board.get_task(task_id)
        except Exception:
            raise HTTPException(status_code=404, detail="Task not found")

        # Add a message noting manual completion
        if hasattr(_runtime, 'conversation_manager') and _runtime.conversation_manager:
            await _runtime.conversation_manager.add_message(
                task_id, "system", "Task manually marked as completed by user"
            )

        await _runtime.task_board.update_status(task_id, TaskStatus.COMPLETED)
        return {"status": "ok", "task_id": task_id}

    @app.post("/api/tasks/{task_id}/fail")
    async def fail_task_manual(task_id: str):
        """Manually mark a task as failed/blocked."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        from xpressai.tasks.board import TaskStatus

        try:
            task = await _runtime.task_board.get_task(task_id)
        except Exception:
            raise HTTPException(status_code=404, detail="Task not found")

        # Add a message noting manual cancellation
        if hasattr(_runtime, 'conversation_manager') and _runtime.conversation_manager:
            await _runtime.conversation_manager.add_message(
                task_id, "system", "Task manually cancelled/failed by user"
            )

        await _runtime.task_board.update_status(task_id, TaskStatus.BLOCKED)
        return {"status": "ok", "task_id": task_id}

    @app.post("/api/tasks/{task_id}/retry")
    async def retry_task(task_id: str):
        """Retry a failed task from scratch.

        Clears the conversation history and resets the task to pending.
        """
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        from xpressai.tasks.board import TaskStatus

        try:
            task = await _runtime.task_board.get_task(task_id)
        except Exception:
            raise HTTPException(status_code=404, detail="Task not found")

        # Clear conversation history
        if hasattr(_runtime, 'conversation_manager') and _runtime.conversation_manager:
            await _runtime.conversation_manager.clear_messages(task_id)

        # Reset task to pending
        await _runtime.task_board.update_status(task_id, TaskStatus.PENDING)
        return {"status": "ok", "task_id": task_id}

    @app.post("/api/tasks/{task_id}/assign")
    async def assign_task(task_id: str, agent_id: str = Form("")):
        """Assign a task to an agent."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        try:
            task = await _runtime.task_board.get_task(task_id)
        except Exception:
            raise HTTPException(status_code=404, detail="Task not found")

        # Empty string means unassigned
        assigned_agent = agent_id if agent_id else None

        await _runtime.task_board.assign_task(task_id, assigned_agent)
        return {"status": "ok", "task_id": task_id, "agent_id": assigned_agent}

    @app.delete("/api/tasks/{task_id}")
    async def delete_task(task_id: str):
        """Delete a specific task."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        try:
            await _runtime.task_board.delete_task(task_id)
        except Exception as e:
            raise HTTPException(status_code=404, detail=f"Task not found or error: {e}")

        return {"status": "ok", "task_id": task_id}

    @app.patch("/api/tasks/{task_id}")
    async def update_task(task_id: str, request: Request):
        """Update a task's title and/or description."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        try:
            form = await request.form()
            title = form.get("title")
            description = form.get("description")

            # Convert empty strings to None for description (to allow clearing)
            if description == "":
                description = None

            task = await _runtime.task_board.update_task(
                task_id,
                title=title if title else None,
                description=description,
            )
            return {"status": "ok", "task_id": task.id, "title": task.title}
        except Exception as e:
            raise HTTPException(status_code=404, detail=f"Task not found or error: {e}")

    @app.delete("/api/tasks/completed/clear")
    async def clear_completed_tasks():
        """Delete all completed tasks."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        from xpressai.tasks.board import TaskStatus

        count = await _runtime.task_board.delete_tasks_by_status(TaskStatus.COMPLETED)
        return {"status": "ok", "deleted_count": count}

    @app.delete("/api/tasks/blocked/clear")
    async def clear_blocked_tasks():
        """Delete all blocked/failed tasks."""
        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        from xpressai.tasks.board import TaskStatus

        count = await _runtime.task_board.delete_tasks_by_status(TaskStatus.BLOCKED)
        return {"status": "ok", "deleted_count": count}

    # Scheduled Tasks API
    @app.get("/api/schedules")
    async def list_schedules():
        """List all scheduled tasks."""
        if not _runtime or not _runtime._scheduler:
            return {"schedules": []}

        schedules = _runtime._scheduler.list_schedules()
        result = []
        for s in schedules:
            next_run = _runtime._scheduler.get_next_run(s.id)
            result.append({
                "id": s.id,
                "name": s.name,
                "cron": s.cron,
                "agent_id": s.agent_id,
                "title": s.title,
                "description": s.description,
                "enabled": s.enabled,
                "last_run": s.last_run.isoformat() if s.last_run else None,
                "run_count": s.run_count,
                "next_run": next_run.isoformat() if next_run else None,
            })
        return {"schedules": result}

    @app.post("/api/schedules")
    async def create_schedule(
        name: str = Form(...),
        cron: str = Form(...),
        agent_id: str = Form(...),
        title: str = Form(...),
        description: Optional[str] = Form(None),
    ):
        """Create a new scheduled task."""
        if not _runtime or not _runtime._scheduler:
            raise HTTPException(status_code=503, detail="Scheduler not available")

        import uuid
        schedule_id = str(uuid.uuid4())[:8]

        try:
            schedule = await _runtime._scheduler.add_schedule(
                schedule_id=schedule_id,
                name=name,
                cron=cron,
                agent_id=agent_id,
                title=title,
                description=description,
            )
            next_run = _runtime._scheduler.get_next_run(schedule_id)
            return {
                "status": "ok",
                "schedule": {
                    "id": schedule.id,
                    "name": schedule.name,
                    "next_run": next_run.isoformat() if next_run else None,
                }
            }
        except Exception as e:
            raise HTTPException(status_code=400, detail=str(e))

    @app.delete("/api/schedules/{schedule_id}")
    async def delete_schedule(schedule_id: str):
        """Delete a scheduled task."""
        if not _runtime or not _runtime._scheduler:
            raise HTTPException(status_code=503, detail="Scheduler not available")

        # Find by prefix match
        schedules = _runtime._scheduler.list_schedules()
        matching = [s for s in schedules if s.id.startswith(schedule_id)]

        if not matching:
            raise HTTPException(status_code=404, detail="Schedule not found")
        if len(matching) > 1:
            raise HTTPException(status_code=400, detail="Multiple schedules match, be more specific")

        await _runtime._scheduler.remove_schedule(matching[0].id)
        return {"status": "ok"}

    @app.post("/api/schedules/{schedule_id}/enable")
    async def enable_schedule(schedule_id: str):
        """Enable a scheduled task."""
        if not _runtime or not _runtime._scheduler:
            raise HTTPException(status_code=503, detail="Scheduler not available")

        success = await _runtime._scheduler.enable_schedule(schedule_id)
        if not success:
            raise HTTPException(status_code=404, detail="Schedule not found")

        # Save to DB
        schedule = _runtime._scheduler.get_schedule(schedule_id)
        if schedule:
            _runtime._scheduler._save_schedule(schedule)

        return {"status": "ok"}

    @app.post("/api/schedules/{schedule_id}/disable")
    async def disable_schedule(schedule_id: str):
        """Disable a scheduled task."""
        if not _runtime or not _runtime._scheduler:
            raise HTTPException(status_code=503, detail="Scheduler not available")

        success = await _runtime._scheduler.disable_schedule(schedule_id)
        if not success:
            raise HTTPException(status_code=404, detail="Schedule not found")

        # Save to DB
        schedule = _runtime._scheduler.get_schedule(schedule_id)
        if schedule:
            _runtime._scheduler._save_schedule(schedule)

        return {"status": "ok"}

    @app.post("/api/schedules/{schedule_id}/trigger")
    async def trigger_schedule(schedule_id: str):
        """Manually trigger a scheduled task immediately."""
        if not _runtime or not _runtime._scheduler:
            raise HTTPException(status_code=503, detail="Scheduler not available")

        task = await _runtime._scheduler.trigger_now(schedule_id)
        if task is None:
            raise HTTPException(status_code=404, detail="Schedule not found")

        return {
            "status": "ok",
            "task": {
                "id": task.id,
                "title": task.title,
            }
        }

    def _cron_to_human(cron: str) -> str:
        """Convert a cron expression to human-readable format."""
        parts = cron.split()
        if len(parts) != 5:
            return cron

        minute, hour, day, month, weekday = parts

        # Common patterns
        if minute == "0" and hour != "*" and day == "*" and month == "*" and weekday == "*":
            h = int(hour)
            ampm = "AM" if h < 12 else "PM"
            h12 = h if h <= 12 else h - 12
            h12 = 12 if h12 == 0 else h12
            return f"Daily at {h12}:{minute.zfill(2)} {ampm}"

        if minute == "0" and hour != "*" and day == "*" and month == "*" and weekday != "*":
            days = {"0": "Sundays", "1": "Mondays", "2": "Tuesdays", "3": "Wednesdays",
                    "4": "Thursdays", "5": "Fridays", "6": "Saturdays", "7": "Sundays"}
            h = int(hour)
            ampm = "AM" if h < 12 else "PM"
            h12 = h if h <= 12 else h - 12
            h12 = 12 if h12 == 0 else h12
            day_name = days.get(weekday, weekday)
            return f"{day_name} at {h12}:{minute.zfill(2)} {ampm}"

        if hour.startswith("*/"):
            interval = hour[2:]
            return f"Every {interval} hours"

        if minute.startswith("*/"):
            interval = minute[2:]
            return f"Every {interval} minutes"

        return cron

    def _format_next_run(next_run, enabled: bool) -> str:
        """Format the next run time in a friendly way."""
        if not enabled:
            return "Paused"
        if not next_run:
            return "N/A"

        from datetime import datetime
        now = datetime.now(next_run.tzinfo) if next_run.tzinfo else datetime.now()
        delta = next_run - now

        if delta.days == 0:
            return next_run.strftime("%I:%M %p").lstrip("0")
        elif delta.days == 1:
            return "Tomorrow " + next_run.strftime("%I:%M %p").lstrip("0")
        elif delta.days < 7:
            return next_run.strftime("%a %I:%M %p").lstrip("0")
        else:
            return next_run.strftime("%b %d %I:%M %p").lstrip("0")

    @app.get("/partials/schedules/count", response_class=HTMLResponse)
    async def schedules_count_partial(request: Request):
        """HTMX partial for active schedules count."""
        if not _runtime or not _runtime._scheduler:
            return HTMLResponse("")

        schedules = _runtime._scheduler.list_schedules()
        active = sum(1 for s in schedules if s.enabled)
        if not schedules:
            return HTMLResponse("")
        return HTMLResponse(f"({active} active)")

    @app.get("/partials/schedules", response_class=HTMLResponse)
    async def schedules_partial(request: Request):
        """HTMX partial for scheduled tasks list."""
        if not _runtime or not _runtime._scheduler:
            return HTMLResponse('<div class="no-schedules">Scheduler not available</div>')

        schedules = _runtime._scheduler.list_schedules()
        if not schedules:
            return HTMLResponse('<div class="no-schedules">No scheduled tasks. Click "+ New Schedule" to create one.</div>')

        html_parts = []
        for s in schedules:
            next_run = _runtime._scheduler.get_next_run(s.id)
            next_run_str = _format_next_run(next_run, s.enabled)
            paused_class = " paused" if not s.enabled else ""
            human_cron = _cron_to_human(s.cron)

            html_parts.append(f'''
                <div class="schedule-card{paused_class}">
                    <div class="schedule-icon">🔄</div>
                    <div class="schedule-content">
                        <div class="schedule-name">{s.name}</div>
                        <div class="schedule-timing">
                            <span class="clock-icon">🕐</span>
                            <span>{human_cron}</span>
                        </div>
                        <div class="schedule-agent">{s.agent_id}</div>
                    </div>
                    <div class="schedule-actions-wrapper">
                        <div class="schedule-buttons">
                            <button class="schedule-btn play"
                                    title="Run now"
                                    hx-post="/api/schedules/{s.id}/trigger"
                                    hx-swap="none"
                                    hx-on::after-request="htmx.trigger('#pending-tasks', 'load')">▶</button>
                            <div class="dropdown" id="dropdown-{s.id}">
                                <button class="schedule-btn" onclick="toggleDropdown(event, 'dropdown-{s.id}')" title="More options">⋯</button>
                                <div class="dropdown-content">
                                    {'<button class="dropdown-item" hx-post="/api/schedules/' + s.id + '/disable" hx-swap="none" hx-on::after-request="htmx.trigger(document.body, \'scheduleUpdate\')">Pause</button>' if s.enabled else '<button class="dropdown-item" hx-post="/api/schedules/' + s.id + '/enable" hx-swap="none" hx-on::after-request="htmx.trigger(document.body, \'scheduleUpdate\')">Resume</button>'}
                                    <button class="dropdown-item danger"
                                            hx-delete="/api/schedules/{s.id}"
                                            hx-swap="none"
                                            hx-confirm="Delete schedule '{s.name}'?"
                                            hx-on::after-request="htmx.trigger(document.body, 'scheduleUpdate')">Delete</button>
                                </div>
                            </div>
                        </div>
                        <div class="schedule-next">Next: {next_run_str}</div>
                    </div>
                </div>
            ''')

        return HTMLResponse("".join(html_parts))

    @app.get("/api/memory/stats")
    async def get_memory_stats():
        """Get memory system stats."""
        if not _runtime or not _runtime.memory_manager:
            return {"error": "Memory not available"}

        return await _runtime.memory_manager.get_stats()

    @app.delete("/api/memory/{memory_id}")
    async def delete_memory(memory_id: str):
        """Delete a memory by ID."""
        if not _runtime or not _runtime.memory_manager:
            return {"error": "Memory not available"}

        try:
            await _runtime.memory_manager.delete(memory_id)
            return {"status": "ok", "deleted": memory_id}
        except Exception as e:
            logger.warning(f"Failed to delete memory {memory_id}: {e}")
            return {"error": str(e)}

    # -------------------------
    # HTMX Partials
    # -------------------------

    @app.get("/partials/agents", response_class=HTMLResponse)
    async def agents_partial(request: Request):
        """HTMX partial for agents list."""
        if not _runtime:
            return HTMLResponse('<div class="empty-state">No runtime available</div>')

        agents = await _runtime.list_agents()

        if not agents:
            return HTMLResponse('<div class="empty-state">No agents configured</div>')

        html_parts = ['<div class="agent-list">']
        for agent in agents:
            status_class = f"status-{agent.status}"
            html_parts.append(f"""
                <div class="agent-card">
                    <span class="agent-status {status_class}"></span>
                    <span class="agent-name">{agent.name}</span>
                    <span class="agent-backend">{agent.backend}</span>
                </div>
            """)

        html_parts.append("</div>")
        return HTMLResponse("".join(html_parts))

    @app.get("/partials/budget", response_class=HTMLResponse)
    async def budget_partial(request: Request):
        """HTMX partial for budget display."""
        if not _runtime:
            return HTMLResponse('<div class="empty-state">Budget tracking not available</div>')

        summary = await _runtime.get_budget_summary()
        total_spent = float(summary.get("total_spent", 0))
        daily_spent = float(summary.get("daily_spent", 0))
        daily_limit = summary.get("daily_limit")
        input_tokens = int(summary.get("input_tokens", 0))
        output_tokens = int(summary.get("output_tokens", 0))
        request_count = int(summary.get("request_count", 0))

        # Format token counts for display
        def format_tokens(n: int) -> str:
            if n >= 1_000_000:
                return f"{n / 1_000_000:.1f}M"
            elif n >= 1_000:
                return f"{n / 1_000:.1f}K"
            return str(n)

        token_info = f"""
            <div class="budget-tokens">
                <span title="Input tokens">{format_tokens(input_tokens)} in</span>
                <span title="Output tokens">{format_tokens(output_tokens)} out</span>
                <span title="Requests">{request_count} reqs</span>
            </div>
        """

        # Get top spenders
        top_spenders = await _runtime.get_top_spenders(3)
        top_spenders_html = ""
        if top_spenders and any(s["total_spent"] > 0 for s in top_spenders):
            spender_items = []
            for s in top_spenders:
                if s["total_spent"] > 0:
                    spender_items.append(
                        f'<div class="spender-item">'
                        f'<span class="spender-name">{s["agent_id"]}</span>'
                        f'<span class="spender-amount">${s["total_spent"]:.2f}</span>'
                        f'</div>'
                    )
            if spender_items:
                top_spenders_html = f"""
                    <div class="top-spenders">
                        <div class="spenders-header">Top Spenders</div>
                        {"".join(spender_items)}
                    </div>
                """

        if daily_limit:
            daily_limit = float(daily_limit)
            pct = (daily_spent / daily_limit * 100) if daily_limit > 0 else 0
            fill_class = ""
            if pct >= 90:
                fill_class = "critical"
            elif pct >= 70:
                fill_class = "warning"

            return HTMLResponse(f"""
                <div class="budget-display">
                    <div class="budget-bar">
                        <div class="budget-fill {fill_class}" style="--progress: {pct}%"></div>
                    </div>
                    <p class="budget-text">
                        Daily: ${daily_spent:.2f} / ${daily_limit:.2f} ({pct:.1f}%)
                    </p>
                    <p class="budget-text">Total: ${total_spent:.2f}</p>
                    {token_info}
                    {top_spenders_html}
                </div>
            """)
        else:
            return HTMLResponse(f"""
                <div class="budget-display">
                    <p class="budget-text">Spent: ${total_spent:.2f}</p>
                    {token_info}
                    {top_spenders_html}
                </div>
            """)

    @app.get("/partials/tasks", response_class=HTMLResponse)
    async def tasks_partial(request: Request):
        """HTMX partial for tasks summary."""
        if not _runtime:
            return HTMLResponse('<div class="empty-state">Tasks not available</div>')

        counts = await _runtime.get_task_counts()
        pending = counts.get("pending", 0)
        in_progress = counts.get("in_progress", 0)
        waiting = counts.get("waiting_for_input", 0)
        completed = counts.get("completed", 0)

        waiting_html = ""
        if waiting > 0:
            waiting_html = f"""
                <div class="task-count waiting">
                    <span class="number">{waiting}</span>
                    <span class="label">Waiting</span>
                </div>
            """

        return HTMLResponse(f"""
            <div class="task-counts">
                <div class="task-count pending">
                    <span class="number">{pending}</span>
                    <span class="label">Pending</span>
                </div>
                <div class="task-count in-progress">
                    <span class="number">{in_progress}</span>
                    <span class="label">In Progress</span>
                </div>
                {waiting_html}
                <div class="task-count completed">
                    <span class="number">{completed}</span>
                    <span class="label">Completed</span>
                </div>
            </div>
        """)

    @app.get("/partials/tasks/done", response_class=HTMLResponse)
    async def tasks_done_partial(request: Request):
        """HTMX partial for done tasks (completed + blocked/failed)."""
        if not _runtime or not _runtime.task_board:
            return HTMLResponse('<div class="empty-state">No tasks</div><span id="done-count" class="task-count" hx-swap-oob="true">0</span>')

        from xpressai.tasks.board import TaskStatus

        # Get both completed and blocked tasks
        completed_tasks = await _runtime.task_board.get_tasks(status=TaskStatus.COMPLETED, limit=20)
        blocked_tasks = await _runtime.task_board.get_tasks(status=TaskStatus.BLOCKED, limit=20)

        all_tasks = completed_tasks + blocked_tasks
        count = len(all_tasks)

        if not all_tasks:
            return HTMLResponse(f'<div class="empty-state">No tasks</div><span id="done-count" class="task-count" hx-swap-oob="true">{count}</span>')

        # Sort by updated_at descending (most recent first)
        all_tasks.sort(key=lambda t: t.updated_at, reverse=True)

        html_parts = []
        for task in all_tasks[:20]:  # Limit to 20 total
            status_class = f"status-{task.status.value}"
            # Add failed class for blocked tasks to get red tint
            failed_class = "task-failed" if task.status == TaskStatus.BLOCKED else ""
            html_parts.append(f"""
                <a href="/task/{task.id}" class="task-card {status_class} {failed_class}">
                    <div class="title">{task.title}</div>
                    <div class="meta">{task.agent_id or 'unassigned'}</div>
                </a>
            """)

        # Add out-of-band swap for the count
        html_parts.append(f'<span id="done-count" class="task-count" hx-swap-oob="true">{count}</span>')

        return HTMLResponse("".join(html_parts))

    @app.get("/partials/tasks/{status}", response_class=HTMLResponse)
    async def tasks_by_status_partial(request: Request, status: str):
        """HTMX partial for tasks by status (for kanban board)."""
        # Map status to count element ID
        count_id_map = {
            "pending": "pending-count",
            "in_progress": "in-progress-count",
            "waiting_for_input": "waiting-count",
        }
        count_id = count_id_map.get(status, f"{status}-count")

        if not _runtime or not _runtime.task_board:
            return HTMLResponse(f'<div class="empty-state">No tasks</div><span id="{count_id}" class="task-count" hx-swap-oob="true">0</span>')

        from xpressai.tasks.board import TaskStatus
        try:
            status_enum = TaskStatus(status)
        except ValueError:
            return HTMLResponse(f'<div class="empty-state">Invalid status: {status}</div>')

        tasks = await _runtime.task_board.get_tasks(status=status_enum, limit=20)
        count = len(tasks)

        if not tasks:
            return HTMLResponse(f'<div class="empty-state">No tasks</div><span id="{count_id}" class="task-count" hx-swap-oob="true">{count}</span>')

        html_parts = []
        for task in tasks:
            status_class = f"status-{task.status.value}"
            html_parts.append(f"""
                <a href="/task/{task.id}" class="task-card {status_class}">
                    <div class="title">{task.title}</div>
                    <div class="meta">{task.agent_id or 'unassigned'}</div>
                </a>
            """)

        # Add out-of-band swap for the count
        html_parts.append(f'<span id="{count_id}" class="task-count" hx-swap-oob="true">{count}</span>')

        return HTMLResponse("".join(html_parts))

    @app.get("/partials/task/{task_id}/messages", response_class=HTMLResponse)
    async def task_messages_partial(request: Request, task_id: str):
        """HTMX partial for task conversation messages."""
        import html as html_module

        if not _runtime:
            return HTMLResponse('<div class="empty-state">Runtime not available</div>')

        try:
            messages = []
            if hasattr(_runtime, 'conversation_manager') and _runtime.conversation_manager:
                messages = await _runtime.conversation_manager.get_messages(task_id)

            if not messages:
                return HTMLResponse('<div class="empty-state">No messages yet</div>')

            html_parts = ['<div class="conversation">']
            for msg in messages:
                timestamp = msg.timestamp.strftime("%H:%M")
                content = msg.content
                escaped_content = html_module.escape(content)
                escaped_content = escaped_content.replace('\n', '<br>')

                # Determine display type based on content patterns
                is_hook = msg.role == "hook"
                is_tool_call = (
                    content.startswith("Calling tool:") or
                    content.startswith("[Tool Call]:") or
                    content.startswith("Tool call:")
                )
                is_tool_result = (
                    msg.role == "tool" or
                    content.startswith("[Tool Result") or
                    (": " in content and not is_tool_call and msg.role == "agent" and
                     any(content.startswith(t) for t in ["read_file:", "write_file:", "list_directory:",
                         "execute_command:", "fetch_url:", "complete_task:", "fail_task:"]))
                )
                is_system = msg.role == "system" or content.startswith("Task prompt") or content.startswith("Task failed") or content.startswith("Task did not")

                if is_hook:
                    hook_name = content.split(":")[0] if ":" in content else "hook"
                    hook_detail = content[len(hook_name)+1:].strip() if ":" in content else content
                    escaped_detail = html_module.escape(hook_detail).replace('\n', '<br>')
                    html_parts.append(f"""
                        <details class="chat-message hook" data-timestamp="{msg.timestamp.isoformat()}">
                            <summary class="hook-summary">
                                <span class="hook-icon">&#9881;</span>
                                <span class="hook-name">{html_module.escape(hook_name)}</span>
                                <span class="meta">{timestamp}</span>
                            </summary>
                            <div class="hook-content">{escaped_detail}</div>
                        </details>
                    """)
                elif is_system or is_tool_result:
                    if is_system:
                        first_line = content.split('\n')[0][:60]
                        summary = f"System: {html_module.escape(first_line)}..."
                    else:
                        if content.startswith("[Tool Result"):
                            match = content.split(']:')[0] if ']:' in content else content[:40]
                            summary = html_module.escape(match.replace('[', '').replace(']', ''))
                        elif ':' in content:
                            tool_name = content.split(':')[0]
                            summary = f"Tool Result: {html_module.escape(tool_name)}"
                        else:
                            summary = "Tool Result"
                    html_parts.append(f"""
                        <details class="message message-tool" data-timestamp="{msg.timestamp.isoformat()}">
                            <summary>
                                <span class="message-summary">{summary}</span>
                                <span class="message-time">{timestamp}</span>
                            </summary>
                            <div class="message-content">{escaped_content}</div>
                        </details>
                    """)
                elif is_tool_call:
                    html_parts.append(f"""
                        <div class="message message-agent">
                            <div class="message-header">
                                <span class="message-role">AGENT</span>
                                <span class="message-time">{timestamp}</span>
                            </div>
                            <div class="message-content">{escaped_content}</div>
                        </div>
                    """)
                else:
                    role_display = msg.role.upper()
                    role_class = f"message-{msg.role}"
                    html_parts.append(f"""
                        <div class="message {role_class}">
                            <div class="message-header">
                                <span class="message-role">{role_display}</span>
                                <span class="message-time">{timestamp}</span>
                            </div>
                            <div class="message-content">{escaped_content}</div>
                        </div>
                    """)

            html_parts.append("</div>")
            return HTMLResponse("".join(html_parts))

        except Exception as e:
            logger.error(f"Error loading task messages: {e}")
            return HTMLResponse(f'<div class="empty-state error">Error loading messages: {html_module.escape(str(e))}</div>')

    @app.get("/partials/memory", response_class=HTMLResponse)
    async def memory_partial(request: Request):
        """HTMX partial for memory stats (dashboard)."""
        if not _runtime or not _runtime.memory_manager:
            return HTMLResponse('<div class="empty-state">Memory not available</div>')

        stats = await _runtime.memory_manager.get_stats()
        zettel = stats.get("zettelkasten", {})
        total = zettel.get("total_memories", 0)
        links = zettel.get("total_links", 0)

        return HTMLResponse(f"""
            <div class="stats-row">
                <div class="stat-item">
                    <span class="stat-value">{total}</span>
                    <span class="stat-label">Memories</span>
                </div>
                <div class="stat-item">
                    <span class="stat-value">{links}</span>
                    <span class="stat-label">Links</span>
                </div>
            </div>
        """)

    @app.get("/partials/memory/stats", response_class=HTMLResponse)
    async def memory_stats_partial(request: Request, agent: str = ""):
        """HTMX partial for memory stats (memory page)."""
        if not _runtime or not _runtime.memory_manager:
            return HTMLResponse('<div class="empty-state">Memory not available</div>')

        # Get count for this agent
        agent_id = agent if agent else None
        memories = await _runtime.memory_manager.get_recent(agent_id=agent_id, limit=1000)
        total = len(memories)

        return HTMLResponse(f"""
            <div class="stats-row">
                <div class="stat-item">
                    <span class="stat-value">{total}</span>
                    <span class="stat-label">Memories</span>
                </div>
            </div>
        """)

    @app.get("/partials/memory/recent", response_class=HTMLResponse)
    async def memory_recent_partial(request: Request, agent: str = ""):
        """HTMX partial for recent memories."""
        if not _runtime or not _runtime.memory_manager:
            return HTMLResponse('<div class="empty-state">Memory not available</div>')

        agent_id = agent if agent else None
        memories = await _runtime.memory_manager.get_recent(agent_id=agent_id, limit=20)

        if not memories:
            return HTMLResponse('<div class="empty-state">No memories yet</div>')

        import html as html_module

        html_parts = ['<div class="memory-list">']
        for memory in memories:
            tags_html = ""
            if memory.tags:
                tags_html = '<div class="tags">' + "".join(
                    f'<span class="tag">#{t}</span>' for t in memory.tags[:3]
                ) + "</div>"

            # Escape content for HTML - handle None values safely
            content_text = memory.content if memory.content else "(no content)"
            summary_text = memory.summary if memory.summary else content_text[:100]
            safe_summary = html_module.escape(summary_text)
            safe_content = html_module.escape(content_text)
            truncated = len(summary_text) > 80
            display_summary = safe_summary[:80] + ('...' if truncated else '')

            html_parts.append(f"""
                <div class="memory-item" data-memory-id="{memory.id}">
                    <div class="memory-header" onclick="toggleMemory(this)">
                        <div class="summary">{display_summary}</div>
                        <div class="memory-actions">
                            <button class="btn-outline-danger"
                                    onclick="event.stopPropagation(); deleteMemory('{memory.id}')"
                                    title="Delete memory">×</button>
                        </div>
                    </div>
                    <div class="memory-details" style="display: none;">
                        <div class="memory-content">{safe_content}</div>
                        <div class="meta">
                            <span class="layer">{memory.layer}</span>
                            <span class="date">{memory.created_at.strftime('%Y-%m-%d %H:%M')}</span>
                            <span class="id">ID: {memory.id[:8]}...</span>
                        </div>
                        {tags_html}
                    </div>
                </div>
            """)

        html_parts.append("</div>")
        return HTMLResponse("".join(html_parts))

    @app.get("/partials/memory/search", response_class=HTMLResponse)
    async def memory_search_partial(request: Request, q: str = "", agent: str = ""):
        """HTMX partial for memory search results."""
        if not _runtime or not _runtime.memory_manager:
            return HTMLResponse('<div class="empty-state">Memory not available</div>')

        if not q:
            return HTMLResponse('<div class="empty-state">Enter a search query</div>')

        agent_id = agent if agent else None
        results = await _runtime.memory_manager.search(q, agent_id=agent_id, limit=20)

        if not results:
            return HTMLResponse(f'<div class="empty-state">No results for "{q}"</div>')

        import html as html_module

        html_parts = ['<div class="memory-list">']
        for result in results:
            memory = result.memory
            score = result.relevance_score
            tags_html = ""
            if memory.tags:
                tags_html = '<div class="tags">' + "".join(
                    f'<span class="tag">#{t}</span>' for t in memory.tags[:3]
                ) + "</div>"

            # Escape content for HTML - handle None values safely
            content_text = memory.content if memory.content else "(no content)"
            summary_text = memory.summary if memory.summary else content_text[:100]
            safe_summary = html_module.escape(summary_text)
            safe_content = html_module.escape(content_text)
            truncated = len(summary_text) > 80
            display_summary = safe_summary[:80] + ('...' if truncated else '')

            html_parts.append(f"""
                <div class="memory-item" data-memory-id="{memory.id}">
                    <div class="memory-header" onclick="toggleMemory(this)">
                        <div class="summary">{display_summary}</div>
                        <div class="memory-actions">
                            <span class="score">Score: {score:.2f}</span>
                            <button class="btn-outline-danger"
                                    onclick="event.stopPropagation(); deleteMemory('{memory.id}')"
                                    title="Delete memory">×</button>
                        </div>
                    </div>
                    <div class="memory-details" style="display: none;">
                        <div class="memory-content">{safe_content}</div>
                        <div class="meta">
                            <span class="layer">{memory.layer}</span>
                            <span class="date">{memory.created_at.strftime('%Y-%m-%d %H:%M')}</span>
                            <span class="id">ID: {memory.id[:8]}...</span>
                        </div>
                        {tags_html}
                    </div>
                </div>
            """)

        html_parts.append("</div>")
        return HTMLResponse("".join(html_parts))

    @app.get("/partials/activity", response_class=HTMLResponse)
    async def activity_partial(request: Request):
        """HTMX partial for recent activity."""
        if not _runtime or not _runtime.activity_manager:
            return HTMLResponse('<div class="empty-state">Activity log not available</div>')

        events = await _runtime.activity_manager.get_recent(limit=20)

        if not events:
            return HTMLResponse('<div class="empty-state">No recent activity</div>')

        # Map event types to icons and colors
        event_icons = {
            "task.created": ("T", "task"),
            "task.started": ("T", "task"),
            "task.completed": ("T", "task"),
            "task.failed": ("T", "task"),
            "task.waiting": ("T", "task"),
            "agent.started": ("A", "agent"),
            "agent.stopped": ("A", "agent"),
            "agent.error": ("A", "agent"),
            "tool.called": ("⚙", "tool"),
            "tool.completed": ("⚙", "tool"),
            "tool.failed": ("⚙", "tool"),
            "system.startup": ("S", "system"),
            "system.shutdown": ("S", "system"),
            "user.message": ("U", "user"),
        }

        def format_event_text(event):
            """Format event into human-readable text."""
            etype = event.event_type.value
            data = event.data or {}

            if etype == "system.startup":
                agents = data.get("agents", [])
                return f"System started with {len(agents)} agent(s)"
            elif etype == "system.shutdown":
                return "System shut down"
            elif etype == "agent.started":
                return f"Agent <strong>{event.agent_id}</strong> started ({data.get('backend', 'unknown')})"
            elif etype == "agent.stopped":
                return f"Agent <strong>{event.agent_id}</strong> stopped"
            elif etype == "agent.error":
                return f"Agent <strong>{event.agent_id}</strong> error"
            elif etype == "task.started":
                title = data.get("title", "Unknown task")
                return f"<strong>{event.agent_id}</strong> started: {title}"
            elif etype == "task.completed":
                title = data.get("title", "Unknown task")
                return f"<strong>{event.agent_id}</strong> completed: {title}"
            elif etype == "task.failed":
                title = data.get("title", "Unknown task")
                reason = data.get("reason", data.get("error", "unknown"))
                return f"<strong>{event.agent_id}</strong> failed: {title}"
            elif etype == "task.waiting":
                return f"<strong>{event.agent_id}</strong> waiting for input"
            else:
                return etype

        html_parts = ['<div class="activity-list">']
        for event in events:
            icon, icon_class = event_icons.get(event.event_type.value, ("•", "system"))
            time_str = event.timestamp.strftime("%H:%M")
            text = format_event_text(event)

            html_parts.append(f"""
                <div class="activity-item">
                    <div class="activity-icon {icon_class}">{icon}</div>
                    <div class="activity-text">{text}</div>
                    <div class="activity-time">{time_str}</div>
                </div>
            """)

        html_parts.append("</div>")
        return HTMLResponse("".join(html_parts))

    @app.get("/partials/logs", response_class=HTMLResponse)
    async def logs_partial(request: Request, agent: str = ""):
        """HTMX partial for activity logs."""
        import html as html_module

        if not _runtime or not _runtime.activity_manager:
            return HTMLResponse('<div class="empty-state">Activity logging not available</div>')

        try:
            # Get activity events
            if agent:
                events = await _runtime.activity_manager.get_by_agent(agent, limit=100)
            else:
                events = await _runtime.activity_manager.get_recent(limit=100)

            if not events:
                return HTMLResponse('<div class="empty-state">No activity logs yet</div>')

            # Build HTML
            html_parts = ['<div class="log-entries">']

            for event in events:
                timestamp = event.timestamp.strftime("%Y-%m-%d %H:%M:%S")
                event_type = event.event_type.value
                agent_id = event.agent_id or "system"

                # Determine log level class based on event type
                if "error" in event_type or "failed" in event_type:
                    level_class = "log-error"
                elif "started" in event_type or "completed" in event_type:
                    level_class = "log-success"
                elif "waiting" in event_type:
                    level_class = "log-warning"
                else:
                    level_class = "log-info"

                # Format event data
                data_str = ""
                if event.data:
                    data_parts = []
                    for key, value in event.data.items():
                        if key in ["task_id", "title", "reason", "error", "summary"]:
                            val_str = str(value)[:100]
                            data_parts.append(f"{key}={html_module.escape(val_str)}")
                    if data_parts:
                        data_str = " | " + ", ".join(data_parts)

                html_parts.append(f"""
                    <div class="log-entry {level_class}">
                        <span class="log-time">{timestamp}</span>
                        <span class="log-agent">[{html_module.escape(agent_id)}]</span>
                        <span class="log-type">{html_module.escape(event_type)}</span>
                        <span class="log-data">{data_str}</span>
                    </div>
                """)

            html_parts.append('</div>')
            return HTMLResponse("".join(html_parts))

        except Exception as e:
            logger.error(f"Error fetching logs: {e}")
            return HTMLResponse(f'<div class="empty-state">Error loading logs: {html_module.escape(str(e))}</div>')

    # -------------------------
    # Agent Chat API Endpoints
    # -------------------------

    def _render_chat_message(row) -> str:
        """Render a single chat message to HTML."""
        import html as html_module

        role = row["role"]
        content = row["content"]
        timestamp = row["timestamp"] or ""

        # Escape HTML in content but preserve newlines
        escaped_content = html_module.escape(content).replace("\n", "<br>")

        time_str = ""
        if timestamp:
            try:
                dt = datetime.fromisoformat(timestamp)
                time_str = dt.strftime("%H:%M")
            except:
                pass

        # Hook messages are collapsible
        if role == "hook":
            # Extract hook name from content (e.g., "memory_recall: ...")
            hook_name = content.split(":")[0] if ":" in content else "hook"
            hook_detail = content[len(hook_name)+1:].strip() if ":" in content else content
            escaped_detail = html_module.escape(hook_detail).replace("\n", "<br>")

            return f"""
                <details class="chat-message hook" data-timestamp="{timestamp}">
                    <summary class="hook-summary">
                        <span class="hook-icon">&#9881;</span>
                        <span class="hook-name">{html_module.escape(hook_name)}</span>
                        <span class="meta">{time_str}</span>
                    </summary>
                    <div class="hook-content">{escaped_detail}</div>
                </details>
            """
        else:
            return f"""
                <div class="chat-message {role}" data-timestamp="{timestamp}">
                    <div class="content">{escaped_content}</div>
                    <div class="meta">{time_str}</div>
                </div>
            """

    @app.get("/partials/agent/{agent_id}/messages", response_class=HTMLResponse)
    async def agent_chat_messages_partial(agent_id: str):
        """HTMX partial for agent chat messages."""
        if not _runtime or not _runtime._db:
            return HTMLResponse('<div class="empty-state">Chat not available</div>')

        # Get chat messages from database
        with _runtime._db.connect() as conn:
            rows = conn.execute(
                """
                SELECT role, content, timestamp FROM agent_chat_messages
                WHERE agent_id = ?
                ORDER BY timestamp ASC
                LIMIT 100
                """,
                (agent_id,),
            ).fetchall()

        if not rows:
            return HTMLResponse(
                '<div class="chat-conversation"><div class="empty-state">Start a conversation...</div></div>'
            )

        html_parts = ['<div class="chat-conversation">']
        for row in rows:
            html_parts.append(_render_chat_message(row))
        html_parts.append("</div>")

        return HTMLResponse("".join(html_parts))

    @app.get("/partials/agent/{agent_id}/messages/new", response_class=HTMLResponse)
    async def agent_chat_messages_new_partial(agent_id: str, after: str = ""):
        """HTMX partial for new agent chat messages (only messages after timestamp)."""
        if not _runtime or not _runtime._db:
            return HTMLResponse("")

        with _runtime._db.connect() as conn:
            if after:
                # Get only new messages after the timestamp
                rows = conn.execute(
                    """
                    SELECT role, content, timestamp FROM agent_chat_messages
                    WHERE agent_id = ? AND timestamp > ?
                    ORDER BY timestamp ASC
                    LIMIT 50
                    """,
                    (agent_id, after),
                ).fetchall()
            else:
                # No timestamp - get recent messages (fallback for first poll)
                rows = conn.execute(
                    """
                    SELECT role, content, timestamp FROM agent_chat_messages
                    WHERE agent_id = ?
                    ORDER BY timestamp DESC
                    LIMIT 20
                    """,
                    (agent_id,),
                ).fetchall()
                # Reverse to get chronological order
                rows = list(reversed(rows))

        if not rows:
            return HTMLResponse("")

        html_parts = []
        for row in rows:
            html_parts.append(_render_chat_message(row))

        return HTMLResponse("".join(html_parts))

    @app.post("/api/agent/{agent_id}/chat")
    async def agent_chat_send(agent_id: str, message: str = Form(...)):
        """Send a message to an agent and get a response."""
        if not _runtime:
            raise HTTPException(status_code=503, detail="Runtime not available")

        agent = await _runtime.get_agent(agent_id)
        if not agent:
            raise HTTPException(status_code=404, detail=f"Agent not found: {agent_id}")

        if agent.status != "running":
            raise HTTPException(status_code=400, detail="Agent is not running")

        # Store user message
        with _runtime._db.connect() as conn:
            conn.execute(
                "INSERT INTO agent_chat_messages (agent_id, role, content) VALUES (?, ?, ?)",
                (agent_id, "user", message),
            )

        # Get the backend for this agent
        backend = _runtime._backends.get(agent_id)
        if not backend:
            raise HTTPException(status_code=500, detail="Agent backend not available")

        # Get agent config for hooks
        agent_config = None
        for ac in _runtime.config.agents:
            if ac.name == agent_id:
                agent_config = ac
                break

        # Run memory recall hook (before_message)
        # Uses the dedicated memory sub-agent backend to avoid polluting main agent's conversation
        memory_context = ""
        memory_backend = None  # Will be set if available
        if (agent_config and agent_config.hooks and
            agent_config.hooks.before_message and
            _runtime.memory_manager and _runtime.config.memory):

            from xpressai.memory.hooks import memory_recall

            # Get memory sub-agent backend (uses same LLM config as main agent by default)
            memory_backend = await _runtime.get_memory_backend(agent_config)

            # Create LLM callback using memory sub-agent (separate from main agent)
            memory_llm_callback = None
            if memory_backend:
                async def memory_llm_callback(prompt: str) -> str:
                    """LLM callback using dedicated memory sub-agent."""
                    # Clear history before each call to keep it stateless
                    memory_backend.clear_history()
                    parts = []
                    async for chunk in memory_backend.send(prompt):
                        parts.append(chunk)
                    return "".join(parts)

            try:
                result = await memory_recall(
                    agent_id=agent_id,
                    message=message,
                    memory_manager=_runtime.memory_manager,
                    memory_config=_runtime.config.memory,
                    llm_callback=memory_llm_callback,
                )

                memory_context = result.get("context", "")
                debug = result.get("debug", {})

                # Build detailed log for hook message
                log_parts = []
                log_parts.append(f"Search query: {debug.get('search_query', 'N/A')}")
                log_parts.append(f"Results found: {debug.get('results_count', 0)}")

                if debug.get("memories"):
                    log_parts.append("\nMemories retrieved:")
                    for mem in debug["memories"]:
                        log_parts.append(f"  - {mem['summary']} (score: {mem['score']:.2f})")

                if debug.get("error"):
                    log_parts.append(f"\nError: {debug['error']}")

                if memory_context:
                    log_parts.append(f"\nContext injected (invisible to agent):\n{memory_context}")

                with _runtime._db.connect() as conn:
                    conn.execute(
                        "INSERT INTO agent_chat_messages (agent_id, role, content) VALUES (?, ?, ?)",
                        (agent_id, "hook", "memory_recall:\n" + "\n".join(log_parts)),
                    )
            except Exception as e:
                logger.error(f"Memory recall hook error: {e}")
                with _runtime._db.connect() as conn:
                    conn.execute(
                        "INSERT INTO agent_chat_messages (agent_id, role, content) VALUES (?, ?, ?)",
                        (agent_id, "hook", f"memory_recall error: {e}"),
                    )

        # Set up meta tools for this chat
        from xpressai.tools.builtin.meta import (
            set_managers,
            get_meta_tool_schemas,
            execute_meta_tool,
        )

        set_managers(
            _runtime.task_board,
            _runtime.memory_manager,
            _runtime.sop_manager,
            agent_id=agent_id,
        )

        # Register meta tools with the backend
        tool_schemas = get_meta_tool_schemas()
        if hasattr(backend, "register_tools"):
            await backend.register_tools(tool_schemas)

        # Inject memory context invisibly into system prompt (agent won't see it explicitly)
        if memory_context and hasattr(backend, "inject_memory"):
            await backend.inject_memory(memory_context)

        try:
            # Check if backend supports native tools
            if hasattr(backend, "_tool_format") and backend._tool_format == "native":
                # Use native tool calling
                response_text, tool_calls = await backend.send_native_with_tools(message)

                # Execute any tool calls
                for tool_name, args, tool_id in tool_calls:
                    result = await execute_meta_tool(tool_name, args)
                    backend.add_tool_result(tool_id, tool_name, result)

                # If there were tool calls, get the final response
                if tool_calls:
                    final_text, _ = await backend.send_native_with_tools(
                        "", is_continuation=True
                    )
                    if final_text:
                        response_text = (response_text + "\n\n" + final_text).strip()

            else:
                # Use streaming response
                response_parts = []
                async for chunk in backend.send(message):
                    response_parts.append(chunk)
                response_text = "".join(response_parts)

            # Store agent response
            with _runtime._db.connect() as conn:
                conn.execute(
                    "INSERT INTO agent_chat_messages (agent_id, role, content) VALUES (?, ?, ?)",
                    (agent_id, "agent", response_text),
                )

            # Clear injected memory context after response (reset to original system message)
            if memory_context and hasattr(backend, "clear_injected_memory"):
                await backend.clear_injected_memory()

            # Run memory remember hook (after_message)
            # Uses the dedicated memory sub-agent backend to avoid polluting main agent's conversation
            if (agent_config and agent_config.hooks and
                agent_config.hooks.after_message and
                _runtime.memory_manager and _runtime.config.memory):

                from xpressai.memory.hooks import memory_remember

                # Get memory backend if not already available
                if memory_backend is None:
                    memory_backend = await _runtime.get_memory_backend(agent_config)

                if memory_backend:
                    # Create LLM callback using memory sub-agent
                    async def memory_remember_callback(prompt: str) -> str:
                        """LLM callback using dedicated memory sub-agent."""
                        memory_backend.clear_history()
                        parts = []
                        async for chunk in memory_backend.send(prompt):
                            parts.append(chunk)
                        return "".join(parts)

                    try:
                        conversation = [
                            {"role": "user", "content": message},
                            {"role": "assistant", "content": response_text},
                        ]

                        remember_result = await memory_remember(
                            agent_id=agent_id,
                            conversation=conversation,
                            memory_manager=_runtime.memory_manager,
                            memory_config=_runtime.config.memory,
                            llm_callback=memory_remember_callback,
                        )

                        # Handle both old bool return and new dict return
                        if isinstance(remember_result, dict):
                            stored = remember_result.get("stored", False)
                            debug = remember_result.get("debug", {})
                        else:
                            stored = remember_result
                            debug = {}

                        # Log hook activity with debug info
                        if stored:
                            hook_msg = f"memory_remember: Stored new memory"
                            if debug.get("memory_id"):
                                hook_msg += f" (id: {debug['memory_id'][:8]}...)"
                        else:
                            # Show debug info about why nothing was stored
                            hook_msg = "memory_remember: Nothing to remember"
                            if debug.get("llm_response"):
                                llm_preview = debug["llm_response"][:150].replace("\n", " ")
                                hook_msg += f"\nLLM said: {llm_preview}..."
                            if debug.get("skipped"):
                                hook_msg += f"\nReason: {debug['skipped']}"
                            if debug.get("parse_error"):
                                hook_msg += f"\nParse error: {debug['parse_error']}"
                            if debug.get("error"):
                                hook_msg += f"\nError: {debug['error']}"
                        with _runtime._db.connect() as conn:
                            conn.execute(
                                "INSERT INTO agent_chat_messages (agent_id, role, content) VALUES (?, ?, ?)",
                                (agent_id, "hook", hook_msg),
                            )
                    except Exception as e:
                        logger.error(f"Memory remember hook error: {e}")
                        with _runtime._db.connect() as conn:
                            conn.execute(
                                "INSERT INTO agent_chat_messages (agent_id, role, content) VALUES (?, ?, ?)",
                                (agent_id, "hook", f"memory_remember error: {e}"),
                            )

            return {"status": "ok", "response": response_text}

        except Exception as e:
            logger.error(f"Agent chat error: {e}")
            # Store error as system message
            with _runtime._db.connect() as conn:
                conn.execute(
                    "INSERT INTO agent_chat_messages (agent_id, role, content) VALUES (?, ?, ?)",
                    (agent_id, "system", f"Error: {str(e)}"),
                )
            raise HTTPException(status_code=500, detail=str(e))

    @app.post("/api/agent/{agent_id}/chat/clear")
    async def agent_chat_clear(agent_id: str):
        """Clear chat history for an agent."""
        if not _runtime or not _runtime._db:
            raise HTTPException(status_code=503, detail="Runtime not available")

        with _runtime._db.connect() as conn:
            conn.execute(
                "DELETE FROM agent_chat_messages WHERE agent_id = ?",
                (agent_id,),
            )

        # Also clear the backend's conversation history
        backend = _runtime._backends.get(agent_id)
        if backend and hasattr(backend, "clear_history"):
            backend.clear_history()

        return {"status": "ok", "message": "Chat history cleared"}

    # Zettelkasten browser routes
    @app.get("/api/zettelkasten/tags")
    async def zettelkasten_tags(agent: str = ""):
        """Get all unique tags."""
        if not _runtime or not _runtime._db:
            return []

        with _runtime._db.connect() as conn:
            sql = """
                SELECT DISTINCT t.tag FROM memory_tags t
                JOIN memories m ON t.memory_id = m.id
                WHERE 1=1
            """
            params = []
            if agent:
                sql += " AND m.agent_id = ?"
                params.append(agent)
            sql += " ORDER BY t.tag"

            rows = conn.execute(sql, params).fetchall()
            return [row["tag"] for row in rows]

    @app.get("/partials/zettelkasten/stats", response_class=HTMLResponse)
    async def zettelkasten_stats_partial(request: Request, agent: str = ""):
        """HTMX partial for zettelkasten stats."""
        if not _runtime or not _runtime._db:
            return HTMLResponse('<div class="empty-state">Not available</div>')

        with _runtime._db.connect() as conn:
            # Count memories
            sql = "SELECT COUNT(*) as cnt FROM memories WHERE 1=1"
            params = []
            if agent:
                sql += " AND agent_id = ?"
                params.append(agent)
            total = conn.execute(sql, params).fetchone()["cnt"]

            # Count links
            links = conn.execute("SELECT COUNT(*) as cnt FROM memory_links").fetchone()["cnt"]

            # Count tags
            tags_sql = """
                SELECT COUNT(DISTINCT t.tag) as cnt FROM memory_tags t
                JOIN memories m ON t.memory_id = m.id WHERE 1=1
            """
            if agent:
                tags_sql += " AND m.agent_id = ?"
            tags = conn.execute(tags_sql, params).fetchone()["cnt"]

        return HTMLResponse(f"""
            <div class="zettel-stat">
                <div class="zettel-stat-value">{total}</div>
                <div class="zettel-stat-label">Memories</div>
            </div>
            <div class="zettel-stat">
                <div class="zettel-stat-value">{links}</div>
                <div class="zettel-stat-label">Links</div>
            </div>
            <div class="zettel-stat">
                <div class="zettel-stat-value">{tags}</div>
                <div class="zettel-stat-label">Tags</div>
            </div>
        """)

    @app.get("/partials/zettelkasten/list", response_class=HTMLResponse)
    async def zettelkasten_list_partial(request: Request, agent: str = "", tag: str = "", q: str = ""):
        """HTMX partial for zettelkasten list."""
        import html as html_module

        if not _runtime or not _runtime._db:
            return HTMLResponse('<div class="empty-state">Not available</div>')

        with _runtime._db.connect() as conn:
            sql = "SELECT * FROM memories WHERE 1=1"
            params = []

            if agent:
                sql += " AND agent_id = ?"
                params.append(agent)

            if q:
                sql += " AND (content LIKE ? OR summary LIKE ?)"
                params.extend([f"%{q}%", f"%{q}%"])

            sql += " ORDER BY created_at DESC LIMIT 100"
            rows = conn.execute(sql, params).fetchall()

            # If filtering by tag, we need to join
            if tag:
                sql = """
                    SELECT m.* FROM memories m
                    JOIN memory_tags t ON m.id = t.memory_id
                    WHERE t.tag = ?
                """
                params = [tag]
                if agent:
                    sql += " AND m.agent_id = ?"
                    params.append(agent)
                if q:
                    sql += " AND (m.content LIKE ? OR m.summary LIKE ?)"
                    params.extend([f"%{q}%", f"%{q}%"])
                sql += " ORDER BY m.created_at DESC LIMIT 100"
                rows = conn.execute(sql, params).fetchall()

        if not rows:
            return HTMLResponse('<div class="empty-state">No memories found</div>')

        html_parts = []
        for row in rows:
            memory_id = row["id"]
            summary = html_module.escape(row["summary"] or "(no summary)")[:80]
            agent_id = row["agent_id"] or "shared"
            created = row["created_at"][:16] if row["created_at"] else ""
            source = row["source"] or ""

            # Get tags for this memory
            with _runtime._db.connect() as conn:
                tags = [r["tag"] for r in conn.execute(
                    "SELECT tag FROM memory_tags WHERE memory_id = ?", (memory_id,)
                ).fetchall()]

            tags_html = "".join(f'<span class="zettel-tag">{html_module.escape(t)}</span>' for t in tags[:3])

            html_parts.append(f"""
                <div class="zettel-item" data-memory-id="{memory_id}" onclick="viewMemory('{memory_id}')">
                    <div class="zettel-item-summary">{summary}</div>
                    <div class="zettel-item-meta">
                        <span>{agent_id}</span>
                        <span>{created}</span>
                        <span>{source}</span>
                    </div>
                    <div class="zettel-item-tags">{tags_html}</div>
                </div>
            """)

        return HTMLResponse("".join(html_parts))

    @app.get("/partials/zettelkasten/detail/{memory_id}", response_class=HTMLResponse)
    async def zettelkasten_detail_partial(request: Request, memory_id: str):
        """HTMX partial for zettelkasten memory detail."""
        import base64
        import html as html_module

        if not _runtime or not _runtime.memory_manager:
            return HTMLResponse('<div class="empty-state">Not available</div>')

        try:
            memory = await _runtime.memory_manager.get(memory_id)
        except Exception as e:
            return HTMLResponse(f'<div class="empty-state">Memory not found: {e}</div>')

        safe_summary = html_module.escape(memory.summary or "(no summary)")
        raw_content = memory.content or "(no content)"
        # Base64 encode the raw content for safe transport and markdown rendering
        content_b64 = base64.b64encode(raw_content.encode('utf-8')).decode('ascii')
        tags_html = "".join(f'<span class="zettel-tag">{html_module.escape(t)}</span>' for t in memory.tags)

        # Format links
        links_html = ""
        if memory.links:
            links_html = '<div class="zettel-detail-section"><h4>Links</h4><div class="zettel-links">'
            for link_id in memory.links:
                try:
                    linked = await _runtime.memory_manager.get(link_id)
                    links_html += f'<div class="zettel-link" onclick="viewMemory(\'{link_id}\')">{html_module.escape(linked.summary[:50])}</div>'
                except Exception:
                    links_html += f'<div class="zettel-link">{link_id[:8]}... (not found)</div>'
            links_html += '</div></div>'

        # Format backlinks
        backlinks_html = ""
        if memory.backlinks:
            backlinks_html = '<div class="zettel-detail-section"><h4>Backlinks</h4><div class="zettel-links">'
            for link_id in memory.backlinks:
                try:
                    linked = await _runtime.memory_manager.get(link_id)
                    backlinks_html += f'<div class="zettel-link" onclick="viewMemory(\'{link_id}\')">{html_module.escape(linked.summary[:50])}</div>'
                except Exception:
                    backlinks_html += f'<div class="zettel-link">{link_id[:8]}... (not found)</div>'
            backlinks_html += '</div></div>'

        return HTMLResponse(f"""
            <div class="zettel-detail-header">
                <div class="zettel-detail-summary">{safe_summary}</div>
                <div class="zettel-item-meta">
                    <span>Agent: {memory.agent_id or 'shared'}</span>
                    <span>Layer: {memory.layer}</span>
                    <span>Source: {memory.source}</span>
                </div>
                <div class="zettel-item-meta">
                    <span>Created: {memory.created_at.strftime('%Y-%m-%d %H:%M')}</span>
                    <span>Accessed: {memory.accessed_at.strftime('%Y-%m-%d %H:%M')}</span>
                    <span>Views: {memory.access_count}</span>
                </div>
                <div class="zettel-item-tags" style="margin-top: 0.5rem;">{tags_html}</div>
            </div>
            <div class="zettel-detail-content" data-raw-content="{content_b64}">Loading...</div>
            {links_html}
            {backlinks_html}
            <div class="zettel-detail-actions">
                <button class="btn-danger" onclick="deleteMemory('{memory_id}')">Delete Memory</button>
            </div>
        """)

    # -------------------------
    # Procedures (SOP) Routes
    # -------------------------

    def _format_sop_description(sop, input_values: dict[str, str]) -> str:
        """Format SOP into a markdown task description with input values substituted."""
        lines = [
            f"## Procedure: {sop.name}",
            "",
            sop.summary,
            "",
        ]

        if input_values:
            lines.append("### Inputs")
            lines.append("| Name | Value |")
            lines.append("|------|-------|")
            for name, value in input_values.items():
                lines.append(f"| {name} | {value} |")
            lines.append("")

        if sop.steps:
            lines.append("### Steps")
            for i, step in enumerate(sop.steps, 1):
                prompt = step.prompt
                # Substitute input values into prompts
                for inp_name, inp_value in input_values.items():
                    prompt = prompt.replace(f"{{{inp_name}}}", inp_value)
                lines.append(f"{i}. {prompt}")
                if step.tools:
                    lines.append(f"   - Tools: {', '.join(step.tools)}")
                if step.inputs:
                    for inp in step.inputs:
                        inp_val = input_values.get(inp, "(not provided)")
                        lines.append(f"   - Uses input: {inp} = {inp_val}")
            lines.append("")

        if sop.outputs:
            lines.append("### Expected Outputs")
            for out in sop.outputs:
                lines.append(f"- {out.name}: {out.context}")

        return "\n".join(lines)

    @app.get("/api/procedures")
    async def list_procedures():
        """List all procedures."""
        from xpressai.tasks.sop import SOPManager

        manager = SOPManager()
        sops = manager.list_sops()
        return {
            "procedures": [
                {
                    "name": sop.name,
                    "summary": sop.summary,
                    "input_count": len(sop.inputs),
                    "step_count": len(sop.steps),
                }
                for sop in sops
            ]
        }

    @app.get("/api/procedures/{name}")
    async def get_procedure(name: str):
        """Get procedure details."""
        from xpressai.tasks.sop import SOPManager

        manager = SOPManager()
        sop = manager.get(name)
        if not sop:
            raise HTTPException(status_code=404, detail="Procedure not found")

        return {
            "name": sop.name,
            "summary": sop.summary,
            "tools": sop.tools,
            "inputs": [
                {"name": inp.name, "context": inp.context, "default": inp.default}
                for inp in sop.inputs
            ],
            "outputs": [
                {"name": out.name, "context": out.context}
                for out in sop.outputs
            ],
            "steps": [
                {
                    "prompt": step.prompt,
                    "tools": step.tools,
                    "inputs": step.inputs,
                }
                for step in sop.steps
            ],
        }

    @app.post("/api/procedures/{name}/run")
    async def run_procedure(name: str, request: Request):
        """Create a task from a procedure."""
        from xpressai.tasks.sop import SOPManager

        if not _runtime or not _runtime.task_board:
            raise HTTPException(status_code=503, detail="Runtime not available")

        manager = SOPManager()
        sop = manager.get(name)
        if not sop:
            raise HTTPException(status_code=404, detail="Procedure not found")

        # Get form data
        form = await request.form()
        agent_id_raw = form.get("agent_id")
        agent_id: str | None = str(agent_id_raw) if agent_id_raw and agent_id_raw != "" else None

        # Collect input values
        input_values: dict[str, str] = {}
        for inp in sop.inputs:
            value = form.get(inp.name)
            if value:
                input_values[inp.name] = str(value)
            elif inp.default:
                input_values[inp.name] = inp.default

        # Format task description
        description = _format_sop_description(sop, input_values)

        # Create task
        task = await _runtime.task_board.create_task(
            title=f"SOP: {sop.name}",
            description=description,
            agent_id=agent_id,
        )

        return {"status": "ok", "task_id": task.id}

    @app.post("/api/procedures")
    async def create_procedure(request: Request):
        """Create a new procedure."""
        import html as html_module
        from xpressai.tasks.sop import SOPManager, SOP, SOPInput, SOPOutput, SOPStep

        form = await request.form()

        name = form.get("name")
        if not name:
            raise HTTPException(status_code=400, detail="Name is required")
        name = str(name).strip()

        summary = str(form.get("summary", "")).strip()
        tools_raw = str(form.get("tools", "")).strip()
        tools = [t.strip() for t in tools_raw.split(",") if t.strip()] if tools_raw else []

        # Parse inputs (JSON array or comma-separated)
        inputs_raw = str(form.get("inputs", "")).strip()
        inputs: list[SOPInput] = []
        if inputs_raw:
            try:
                import json
                inputs_data = json.loads(inputs_raw)
                for inp in inputs_data:
                    inputs.append(SOPInput(
                        name=inp.get("name", ""),
                        context=inp.get("context", ""),
                        default=inp.get("default"),
                    ))
            except json.JSONDecodeError:
                # Treat as simple comma-separated names
                for inp_name in inputs_raw.split(","):
                    inp_name = inp_name.strip()
                    if inp_name:
                        inputs.append(SOPInput(name=inp_name, context=""))

        # Parse outputs (JSON array or comma-separated)
        outputs_raw = str(form.get("outputs", "")).strip()
        outputs: list[SOPOutput] = []
        if outputs_raw:
            try:
                import json
                outputs_data = json.loads(outputs_raw)
                for out in outputs_data:
                    outputs.append(SOPOutput(
                        name=out.get("name", ""),
                        context=out.get("context", ""),
                    ))
            except json.JSONDecodeError:
                # Treat as simple comma-separated names
                for out_name in outputs_raw.split(","):
                    out_name = out_name.strip()
                    if out_name:
                        outputs.append(SOPOutput(name=out_name, context=""))

        # Parse steps (JSON array)
        steps_raw = str(form.get("steps", "")).strip()
        steps: list[SOPStep] = []
        if steps_raw:
            try:
                import json
                steps_data = json.loads(steps_raw)
                for step in steps_data:
                    step_tools = step.get("tools", [])
                    if isinstance(step_tools, str):
                        step_tools = [t.strip() for t in step_tools.split(",") if t.strip()]
                    step_inputs = step.get("inputs", [])
                    if isinstance(step_inputs, str):
                        step_inputs = [i.strip() for i in step_inputs.split(",") if i.strip()]
                    steps.append(SOPStep(
                        prompt=step.get("prompt", ""),
                        tools=step_tools,
                        inputs=step_inputs,
                    ))
            except json.JSONDecodeError:
                raise HTTPException(status_code=400, detail="Invalid steps JSON format")

        # Create SOP
        sop = SOP(
            name=name,
            summary=summary,
            tools=tools,
            inputs=inputs,
            outputs=outputs,
            steps=steps,
        )

        manager = SOPManager()
        try:
            path = manager.create(sop)
            return {"status": "ok", "name": sop.name, "path": str(path)}
        except Exception as e:
            raise HTTPException(status_code=500, detail=f"Failed to create procedure: {e}")

    @app.put("/api/procedures/{name}")
    async def update_procedure(name: str, request: Request):
        """Update an existing procedure."""
        import html as html_module
        from xpressai.tasks.sop import SOPManager, SOP, SOPInput, SOPOutput, SOPStep

        manager = SOPManager()

        # Check if procedure exists
        existing = manager.get(name)
        if not existing:
            raise HTTPException(status_code=404, detail="Procedure not found")

        form = await request.form()

        new_name = form.get("name")
        if not new_name:
            raise HTTPException(status_code=400, detail="Name is required")
        new_name = str(new_name).strip()

        summary = str(form.get("summary", "")).strip()
        tools_raw = str(form.get("tools", "")).strip()
        tools = [t.strip() for t in tools_raw.split(",") if t.strip()] if tools_raw else []

        # Parse inputs
        inputs_raw = str(form.get("inputs", "")).strip()
        inputs: list[SOPInput] = []
        if inputs_raw:
            try:
                import json
                inputs_data = json.loads(inputs_raw)
                for inp in inputs_data:
                    inputs.append(SOPInput(
                        name=inp.get("name", ""),
                        context=inp.get("context", ""),
                        default=inp.get("default"),
                    ))
            except json.JSONDecodeError:
                for inp_name in inputs_raw.split(","):
                    inp_name = inp_name.strip()
                    if inp_name:
                        inputs.append(SOPInput(name=inp_name, context=""))

        # Parse outputs
        outputs_raw = str(form.get("outputs", "")).strip()
        outputs: list[SOPOutput] = []
        if outputs_raw:
            try:
                import json
                outputs_data = json.loads(outputs_raw)
                for out in outputs_data:
                    outputs.append(SOPOutput(
                        name=out.get("name", ""),
                        context=out.get("context", ""),
                    ))
            except json.JSONDecodeError:
                for out_name in outputs_raw.split(","):
                    out_name = out_name.strip()
                    if out_name:
                        outputs.append(SOPOutput(name=out_name, context=""))

        # Parse steps
        steps_raw = str(form.get("steps", "")).strip()
        steps: list[SOPStep] = []
        if steps_raw:
            try:
                import json
                steps_data = json.loads(steps_raw)
                for step in steps_data:
                    step_tools = step.get("tools", [])
                    if isinstance(step_tools, str):
                        step_tools = [t.strip() for t in step_tools.split(",") if t.strip()]
                    step_inputs = step.get("inputs", [])
                    if isinstance(step_inputs, str):
                        step_inputs = [i.strip() for i in step_inputs.split(",") if i.strip()]
                    steps.append(SOPStep(
                        prompt=step.get("prompt", ""),
                        tools=step_tools,
                        inputs=step_inputs,
                    ))
            except json.JSONDecodeError:
                raise HTTPException(status_code=400, detail="Invalid steps JSON format")

        # Create updated SOP
        sop = SOP(
            name=new_name,
            summary=summary,
            tools=tools,
            inputs=inputs,
            outputs=outputs,
            steps=steps,
        )

        try:
            # Delete old and create new (handles name changes)
            manager.delete(name)
            path = manager.create(sop)
            return {"status": "ok", "name": sop.name, "path": str(path)}
        except Exception as e:
            raise HTTPException(status_code=500, detail=f"Failed to update procedure: {e}")

    @app.delete("/api/procedures/{name}")
    async def delete_procedure(name: str):
        """Delete a procedure."""
        from xpressai.tasks.sop import SOPManager

        manager = SOPManager()

        # Check if procedure exists
        existing = manager.get(name)
        if not existing:
            raise HTTPException(status_code=404, detail="Procedure not found")

        if manager.delete(name):
            return {"status": "ok", "deleted": name}
        else:
            raise HTTPException(status_code=500, detail="Failed to delete procedure")

    @app.get("/partials/procedures/list", response_class=HTMLResponse)
    async def procedures_list_partial(request: Request):
        """HTMX partial for procedures list."""
        import html as html_module
        from xpressai.tasks.sop import SOPManager

        manager = SOPManager()
        sops = manager.list_sops()

        if not sops:
            return HTMLResponse('<div class="empty-state">No procedures found</div>')

        html_parts = []
        for sop in sops:
            safe_name = html_module.escape(sop.name)
            safe_summary = html_module.escape(sop.summary or "(no summary)")[:60]
            input_count = len(sop.inputs)
            step_count = len(sop.steps)

            html_parts.append(f"""
                <div class="procedure-item" data-name="{safe_name}"
                     hx-get="/partials/procedures/{sop.name}"
                     hx-target="#procedure-detail"
                     hx-swap="innerHTML"
                     onclick="selectProcedure(this)">
                    <div class="procedure-item-name">{safe_name}</div>
                    <div class="procedure-item-summary">{safe_summary}</div>
                    <div class="procedure-item-meta">
                        <span>{input_count} input{"s" if input_count != 1 else ""}</span>
                        <span>{step_count} step{"s" if step_count != 1 else ""}</span>
                    </div>
                </div>
            """)

        return HTMLResponse("".join(html_parts))

    @app.get("/partials/procedures/create-form", response_class=HTMLResponse)
    async def procedure_create_form_partial(request: Request):
        """HTMX partial for procedure create form."""
        import html as html_module

        # Get agents for selection
        agents_options = '<option value="">Unassigned</option>'
        if _runtime:
            agents = await _runtime.list_agents()
            for agent in agents:
                escaped_name = html_module.escape(agent.name)
                agents_options += f'<option value="{escaped_name}">{escaped_name}</option>'

        return HTMLResponse(f"""
            <div class="procedure-form-container">
                <h3>Create New Procedure</h3>
                <form id="create-procedure-form" onsubmit="handleCreateProcedure(event)">
                    <div class="form-field">
                        <label for="proc-name">Name *</label>
                        <input type="text" id="proc-name" name="name" required
                               placeholder="My Procedure">
                    </div>
                    <div class="form-field">
                        <label for="proc-summary">Summary</label>
                        <textarea id="proc-summary" name="summary" rows="2"
                                  placeholder="Brief description of what this procedure does"></textarea>
                    </div>
                    <div class="form-field">
                        <label for="proc-tools">Tools (comma-separated)</label>
                        <input type="text" id="proc-tools" name="tools"
                               placeholder="read_file, write_file, execute_command">
                    </div>
                    
                    <div class="form-section">
                        <h4>Inputs</h4>
                        <div id="inputs-container"></div>
                        <button type="button" class="btn-small" onclick="addInput()">+ Add Input</button>
                    </div>
                    
                    <div class="form-section">
                        <h4>Steps</h4>
                        <div id="steps-container"></div>
                        <button type="button" class="btn-small" onclick="addStep()">+ Add Step</button>
                    </div>
                    
                    <div class="form-section">
                        <h4>Outputs</h4>
                        <div id="outputs-container"></div>
                        <button type="button" class="btn-small" onclick="addOutput()">+ Add Output</button>
                    </div>
                    
                    <div class="form-actions">
                        <button type="button" class="btn-secondary" onclick="closeCreateForm()">Cancel</button>
                        <button type="submit" class="btn-primary">Create Procedure</button>
                    </div>
                </form>
            </div>
        """)

    @app.get("/partials/procedures/{name}", response_class=HTMLResponse)
    async def procedure_detail_partial(request: Request, name: str):
        """HTMX partial for procedure detail."""
        import html as html_module
        from xpressai.tasks.sop import SOPManager

        manager = SOPManager()
        sop = manager.get(name)

        if not sop:
            return HTMLResponse('<div class="empty-state">Procedure not found</div>')

        safe_name = html_module.escape(sop.name)
        safe_summary = html_module.escape(sop.summary or "(no summary)")

        # Tools section
        tools_html = ""
        if sop.tools:
            tools_html = '<div class="detail-section"><h4>Tools</h4><div class="tools-list">'
            for tool in sop.tools:
                tools_html += f'<span class="tool-tag">{html_module.escape(tool)}</span>'
            tools_html += '</div></div>'

        # Inputs section
        inputs_html = ""
        if sop.inputs:
            inputs_html = '<div class="detail-section"><h4>Inputs</h4><ul class="inputs-list">'
            for inp in sop.inputs:
                default_html = ""
                if inp.default:
                    escaped_default = html_module.escape(inp.default)
                    default_html = f'<div class="input-default">Default: {escaped_default}</div>'
                inputs_html += f"""
                    <li class="input-item">
                        <div class="input-name">{html_module.escape(inp.name)}</div>
                        <div class="input-context">{html_module.escape(inp.context or "")}</div>
                        {default_html}
                    </li>
                """
            inputs_html += '</ul></div>'

        # Steps section
        steps_html = ""
        if sop.steps:
            steps_html = '<div class="detail-section"><h4>Steps</h4><ol class="steps-list">'
            for step in sop.steps:
                step_tools = ""
                if step.tools:
                    step_tools = '<div class="step-tools">Tools: ' + ", ".join(
                        html_module.escape(t) for t in step.tools
                    ) + '</div>'
                step_inputs = ""
                if step.inputs:
                    step_inputs = '<div class="step-inputs">Uses: ' + ", ".join(
                        html_module.escape(i) for i in step.inputs
                    ) + '</div>'
                steps_html += f"""
                    <li class="step-item">
                        <div class="step-prompt">{html_module.escape(step.prompt)}</div>
                        {step_tools}
                        {step_inputs}
                    </li>
                """
            steps_html += '</ol></div>'

        # Outputs section
        outputs_html = ""
        if sop.outputs:
            outputs_html = '<div class="detail-section">'
            outputs_html += '<h4>Expected Outputs</h4><ul class="outputs-list">'
            for out in sop.outputs:
                outputs_html += f"""
                    <li class="output-item">
                        <div class="output-name">{html_module.escape(out.name)}</div>
                        <div class="output-context">{html_module.escape(out.context or "")}</div>
                    </li>
                """
            outputs_html += '</ul></div>'

        return HTMLResponse(f"""
            <div class="procedure-detail-header">
                <h3>{safe_name}</h3>
                <p class="procedure-summary">{safe_summary}</p>
            </div>
            {tools_html}
            {inputs_html}
            {steps_html}
            {outputs_html}
            <div class="procedure-actions">
                <button class="btn-primary"
                        hx-get="/partials/procedures/{name}/run-form"
                        hx-target="#run-form-container"
                        hx-swap="innerHTML">
                    Run Procedure
                </button>
                <button class="btn-secondary"
                        hx-get="/partials/procedures/{name}/edit-form"
                        hx-target="#run-form-container"
                        hx-swap="innerHTML">
                    Edit
                </button>
                <button class="btn-danger"
                        onclick="deleteProcedure('{name}')">
                    Delete
                </button>
            </div>
            <div id="run-form-container"></div>
        """)

    @app.get("/partials/procedures/{name}/run-form", response_class=HTMLResponse)
    async def procedure_run_form_partial(request: Request, name: str):
        """HTMX partial for procedure run form."""
        import html as html_module
        from xpressai.tasks.sop import SOPManager

        manager = SOPManager()
        sop = manager.get(name)

        if not sop:
            return HTMLResponse('<div class="empty-state">Procedure not found</div>')

        # Build input fields
        inputs_html = ""
        if sop.inputs:
            for inp in sop.inputs:
                default_val = html_module.escape(inp.default or "") if inp.default else ""
                context_html = ""
                if inp.context:
                    context_html = f'<p class="help-text">{html_module.escape(inp.context)}</p>'
                inputs_html += f"""
                    <div class="form-field">
                        <label for="input_{inp.name}">{html_module.escape(inp.name)}</label>
                        {context_html}
                        <input type="text"
                               id="input_{inp.name}"
                               name="{html_module.escape(inp.name)}"
                               value="{default_val}"
                               placeholder="Enter value...">
                    </div>
                """

        # Get agents for selection
        agents_options = '<option value="">Unassigned</option>'
        if _runtime:
            agents = await _runtime.list_agents()
            for agent in agents:
                escaped_name = html_module.escape(agent.name)
                agents_options += f'<option value="{escaped_name}">{escaped_name}</option>'

        safe_name = html_module.escape(sop.name)

        return HTMLResponse(f"""
            <div class="run-form-modal">
                <h4>Run: {safe_name}</h4>
                <form hx-post="/api/procedures/{name}/run"
                      hx-swap="none"
                      onsubmit="handleProcedureRun(event)">
                    {inputs_html}
                    <div class="form-field">
                        <label for="agent_id">Assign to Agent</label>
                        <select id="agent_id" name="agent_id">
                            {agents_options}
                        </select>
                    </div>
                    <div class="form-actions">
                        <button type="button" class="btn-secondary"
                                onclick="closeRunForm()">Cancel</button>
                        <button type="submit" class="btn-primary">Create Task</button>
                    </div>
                </form>
            </div>
        """)

    @app.get("/partials/procedures/{name}/edit-form", response_class=HTMLResponse)
    async def procedure_edit_form_partial(request: Request, name: str):
        """HTMX partial for procedure edit form."""
        import html as html_module
        import json
        from xpressai.tasks.sop import SOPManager

        manager = SOPManager()
        sop = manager.get(name)

        if not sop:
            return HTMLResponse('<div class="empty-state">Procedure not found</div>')

        safe_name = html_module.escape(sop.name)
        safe_summary = html_module.escape(sop.summary or "")
        tools_str = html_module.escape(", ".join(sop.tools) if sop.tools else "")

        # Pre-populate inputs
        inputs_html = ""
        for i, inp in enumerate(sop.inputs):
            inp_name = html_module.escape(inp.name)
            inp_context = html_module.escape(inp.context or "")
            inp_default = html_module.escape(inp.default or "")
            inputs_html += f"""
                <div class="input-entry" data-index="{i}">
                    <input type="text" placeholder="Name" value="{inp_name}" class="input-name">
                    <input type="text" placeholder="Description" value="{inp_context}" class="input-context">
                    <input type="text" placeholder="Default" value="{inp_default}" class="input-default">
                    <button type="button" class="btn-remove" onclick="removeEntry(this)">×</button>
                </div>
            """

        # Pre-populate steps
        steps_html = ""
        for i, step in enumerate(sop.steps):
            step_prompt = html_module.escape(step.prompt)
            step_tools = html_module.escape(", ".join(step.tools) if step.tools else "")
            step_inputs = html_module.escape(", ".join(step.inputs) if step.inputs else "")
            steps_html += f"""
                <div class="step-entry" data-index="{i}">
                    <textarea placeholder="Step prompt" class="step-prompt" rows="2">{step_prompt}</textarea>
                    <input type="text" placeholder="Tools (comma-separated)" value="{step_tools}" class="step-tools">
                    <input type="text" placeholder="Inputs used (comma-separated)" value="{step_inputs}" class="step-inputs">
                    <button type="button" class="btn-remove" onclick="removeEntry(this)">×</button>
                </div>
            """

        # Pre-populate outputs
        outputs_html = ""
        for i, out in enumerate(sop.outputs):
            out_name = html_module.escape(out.name)
            out_context = html_module.escape(out.context or "")
            outputs_html += f"""
                <div class="output-entry" data-index="{i}">
                    <input type="text" placeholder="Name" value="{out_name}" class="output-name">
                    <input type="text" placeholder="Description" value="{out_context}" class="output-context">
                    <button type="button" class="btn-remove" onclick="removeEntry(this)">×</button>
                </div>
            """

        return HTMLResponse(f"""
            <div class="procedure-form-container">
                <h3>Edit Procedure</h3>
                <form id="edit-procedure-form" onsubmit="handleEditProcedure(event, '{name}')">
                    <div class="form-field">
                        <label for="proc-name">Name *</label>
                        <input type="text" id="proc-name" name="name" required
                               value="{safe_name}">
                    </div>
                    <div class="form-field">
                        <label for="proc-summary">Summary</label>
                        <textarea id="proc-summary" name="summary" rows="2">{safe_summary}</textarea>
                    </div>
                    <div class="form-field">
                        <label for="proc-tools">Tools (comma-separated)</label>
                        <input type="text" id="proc-tools" name="tools" value="{tools_str}">
                    </div>
                    
                    <div class="form-section">
                        <h4>Inputs</h4>
                        <div id="inputs-container">{inputs_html}</div>
                        <button type="button" class="btn-small" onclick="addInput()">+ Add Input</button>
                    </div>
                    
                    <div class="form-section">
                        <h4>Steps</h4>
                        <div id="steps-container">{steps_html}</div>
                        <button type="button" class="btn-small" onclick="addStep()">+ Add Step</button>
                    </div>
                    
                    <div class="form-section">
                        <h4>Outputs</h4>
                        <div id="outputs-container">{outputs_html}</div>
                        <button type="button" class="btn-small" onclick="addOutput()">+ Add Output</button>
                    </div>
                    
                    <div class="form-actions">
                        <button type="button" class="btn-secondary" onclick="closeRunForm()">Cancel</button>
                        <button type="submit" class="btn-primary">Save Changes</button>
                    </div>
                </form>
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
        .budget-tokens {
            display: flex;
            gap: 1rem;
            font-size: 0.75rem;
            color: var(--fg-muted, #8b949e);
            margin-top: 0.5rem;
        }
        .budget-tokens span {
            padding: 0.125rem 0.5rem;
            background: rgba(255,255,255,0.05);
            border-radius: 4px;
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


def run_web(runtime: Runtime | None = None, host: str = "127.0.0.1", port: int = 8935) -> None:
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
