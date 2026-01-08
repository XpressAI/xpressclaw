"""Agent management routes.

Contains page routes and API routes for agent CRUD operations.
Chat functionality is in routes/chat.py.
"""

from __future__ import annotations

import logging
import re
from pathlib import Path
from typing import Optional

import yaml
from fastapi import APIRouter, Request, HTTPException, Form
from fastapi.responses import HTMLResponse

from xpressai.web.deps import get_runtime, get_templates

logger = logging.getLogger(__name__)

router = APIRouter()


# -------------------------
# Page Routes
# -------------------------

@router.get("/agents", response_class=HTMLResponse)
async def agents_page(request: Request):
    """Agents list page."""
    runtime = get_runtime()
    templates = get_templates()

    agents = []
    agent_configs = {}
    agent_budgets = {}

    if runtime:
        agents = await runtime.list_agents()

        # Load agent configs from yaml for model/backend info
        config_path = Path.cwd() / "xpressai.yaml"
        if config_path.exists():
            with open(config_path) as f:
                config_data = yaml.safe_load(f) or {}
            for agent_cfg in config_data.get("agents", []):
                agent_configs[agent_cfg.get("name")] = agent_cfg

        # Get budget info for each agent
        for agent in agents:
            budget = await runtime.get_budget_summary(agent.name)
            agent_budgets[agent.name] = budget

    if templates:
        return templates.TemplateResponse(
            "agents.html",
            {"request": request, "agents": agents, "agent_configs": agent_configs, "agent_budgets": agent_budgets, "active": "agents"},
        )
    return HTMLResponse("<h1>Agents - Templates not installed</h1>")


@router.get("/agents/new", response_class=HTMLResponse)
async def new_agent_page(request: Request):
    """New agent creation page."""
    templates = get_templates()
    if templates:
        return templates.TemplateResponse(
            "agent_new.html",
            {"request": request, "active": "agents"},
        )
    return HTMLResponse("<h1>New Agent - Templates not installed</h1>")


@router.get("/agent/{agent_id}/edit", response_class=HTMLResponse)
async def agent_edit_page(request: Request, agent_id: str):
    """Agent edit page."""
    runtime = get_runtime()
    templates = get_templates()

    agent = None
    agent_config = None
    if runtime:
        agent = await runtime.get_agent(agent_id)

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


@router.get("/agent/{agent_id}/chat", response_class=HTMLResponse)
async def agent_chat_page(request: Request, agent_id: str):
    """Agent chat page."""
    runtime = get_runtime()
    templates = get_templates()

    agent = None
    if runtime:
        agent = await runtime.get_agent(agent_id)

    if not agent:
        raise HTTPException(status_code=404, detail=f"Agent not found: {agent_id}")

    if templates:
        return templates.TemplateResponse(
            "agent_chat.html",
            {"request": request, "agent": agent, "active": "agents"},
        )
    return HTMLResponse("<h1>Agent Chat - Templates not installed</h1>")


# -------------------------
# API Routes
# -------------------------

@router.get("/api/agents")
async def list_agents():
    """List all agents."""
    runtime = get_runtime()
    if not runtime:
        return {"agents": []}

    agents = await runtime.list_agents()
    return {
        "agents": [{"name": a.name, "status": a.status, "backend": a.backend} for a in agents]
    }


@router.get("/api/agents/{agent_name}")
async def get_agent(agent_name: str):
    """Get details for a specific agent."""
    runtime = get_runtime()
    if not runtime:
        raise HTTPException(status_code=503, detail="Runtime not available")

    agent = await runtime.get_agent(agent_name)
    if not agent:
        raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")

    return {"name": agent.name, "status": agent.status, "backend": agent.backend}


@router.post("/api/agents/{agent_name}/start")
async def start_agent(agent_name: str):
    """Start a specific agent."""
    runtime = get_runtime()
    if not runtime:
        raise HTTPException(status_code=503, detail="Runtime not available")

    try:
        agent = await runtime.start_agent(agent_name)
        return {"success": True, "name": agent.name, "status": agent.status}
    except Exception as e:
        error_msg = str(e)
        if "not found" in error_msg.lower() or "unknown agent" in error_msg.lower():
            raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")
        if "already running" in error_msg.lower():
            raise HTTPException(status_code=400, detail=f"Agent '{agent_name}' is already running")
        raise HTTPException(status_code=500, detail=error_msg)


@router.post("/api/agents/{agent_name}/stop")
async def stop_agent(agent_name: str):
    """Stop a specific agent."""
    runtime = get_runtime()
    if not runtime:
        raise HTTPException(status_code=503, detail="Runtime not available")

    try:
        agent = await runtime.stop_agent(agent_name)
        return {"success": True, "name": agent.name, "status": agent.status}
    except Exception as e:
        error_msg = str(e)
        if "not found" in error_msg.lower() or "unknown agent" in error_msg.lower():
            raise HTTPException(status_code=404, detail=f"Agent '{agent_name}' not found")
        raise HTTPException(status_code=500, detail=error_msg)


@router.post("/api/agents")
async def create_agent(request: Request):
    """Create a new agent and save to xpressai.yaml."""
    runtime = get_runtime()

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
    if runtime:
        runtime.reload_config()

    return {"success": True, "agent": {"name": name, "backend": backend, "status": "stopped"}}


@router.put("/api/agents/{agent_name}")
async def update_agent(agent_name: str, request: Request):
    """Update an existing agent's configuration."""
    runtime = get_runtime()

    # Check if agent is running
    if runtime:
        agent = await runtime.get_agent(agent_name)
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
    if runtime:
        runtime.reload_config()

    return {"success": True, "agent": {"name": agent_name}}


@router.delete("/api/agents/{agent_name}")
async def delete_agent(agent_name: str):
    """Delete an agent from the configuration."""
    runtime = get_runtime()

    # Check if agent is running
    if runtime:
        agent = await runtime.get_agent(agent_name)
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

    # Reload config
    if runtime:
        runtime.reload_config()

    return {"success": True, "message": f"Agent '{agent_name}' deleted"}


# -------------------------
# HTMX Partials
# -------------------------

@router.get("/partials/agents", response_class=HTMLResponse)
async def agents_partial(request: Request):
    """HTMX partial for agents list (dashboard)."""
    runtime = get_runtime()
    if not runtime:
        return HTMLResponse('<div class="empty-state">No runtime available</div>')

    agents = await runtime.list_agents()

    if not agents:
        return HTMLResponse('<div class="empty-state">No agents configured</div>')

    html_parts = []
    for agent in agents:
        status_class = f"status-{agent.status}"
        html_parts.append(f"""
            <div class="agent-item {status_class}">
                <div class="name">{agent.name}</div>
                <div class="meta">{agent.backend} - {agent.status}</div>
            </div>
        """)

    return HTMLResponse("".join(html_parts))
