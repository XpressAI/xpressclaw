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

# Import context management for dynamic history
from xpressai.memory.context import (
    count_tokens,
    ContextManager,
    Message,
    MessageEmbedder,
    get_message_embedder,
    set_message_embedder,
)

# Import shared dependencies
from xpressai.web.deps import (
    get_runtime,
    set_runtime,
    get_templates,
    set_templates,
    render_markdown,
    FASTAPI_AVAILABLE,
)

# Import route modules
from xpressai.web.routes.tasks import router as tasks_router
from xpressai.web.routes.agents import router as agents_router
from xpressai.web.routes.memory import router as memory_router

try:
    from fastapi import FastAPI, Request, HTTPException, Form
    from fastapi.responses import HTMLResponse, RedirectResponse
    from fastapi.templating import Jinja2Templates
    from fastapi.staticfiles import StaticFiles
    from pydantic import BaseModel

except ImportError:
    FastAPI = None  # type: ignore
    BaseModel = object  # type: ignore


# Keep local reference for backwards compatibility within this file
_runtime: Runtime | None = None


def _render_markdown(text: str) -> str:
    """Simple markdown rendering without external dependencies."""
    import html as html_module
    import re

    # Escape HTML first for security
    text = html_module.escape(text)

    # Code blocks (``` ... ```)
    text = re.sub(
        r'```(\w*)\n(.*?)```',
        lambda m: f'<pre><code class="language-{m.group(1)}">{m.group(2)}</code></pre>',
        text,
        flags=re.DOTALL
    )

    # Inline code
    text = re.sub(r'`([^`]+)`', r'<code>\1</code>', text)

    # Headers
    text = re.sub(r'^### (.+)$', r'<h3>\1</h3>', text, flags=re.MULTILINE)
    text = re.sub(r'^## (.+)$', r'<h2>\1</h2>', text, flags=re.MULTILINE)
    text = re.sub(r'^# (.+)$', r'<h1>\1</h1>', text, flags=re.MULTILINE)

    # Bold and italic
    text = re.sub(r'\*\*\*(.+?)\*\*\*', r'<strong><em>\1</em></strong>', text)
    text = re.sub(r'\*\*(.+?)\*\*', r'<strong>\1</strong>', text)
    text = re.sub(r'\*(.+?)\*', r'<em>\1</em>', text)

    # Links
    text = re.sub(r'\[([^\]]+)\]\(([^)]+)\)', r'<a href="\2" target="_blank">\1</a>', text)

    # Horizontal rules
    text = re.sub(r'^---+$', r'<hr>', text, flags=re.MULTILINE)

    # Lists (simple)
    text = re.sub(r'^(\d+)\. (.+)$', r'<li>\2</li>', text, flags=re.MULTILINE)
    text = re.sub(r'^- (.+)$', r'<li>\1</li>', text, flags=re.MULTILINE)

    # Tables (basic support)
    lines = text.split('\n')
    in_table = False
    result_lines = []
    for line in lines:
        if '|' in line and not line.strip().startswith('<'):
            cells = [c.strip() for c in line.split('|')[1:-1]]
            if cells:
                if all(c.replace('-', '') == '' for c in cells):
                    continue  # Skip separator row
                if not in_table:
                    result_lines.append('<table>')
                    in_table = True
                result_lines.append('<tr>' + ''.join(f'<td>{c}</td>' for c in cells) + '</tr>')
            else:
                result_lines.append(line)
        else:
            if in_table:
                result_lines.append('</table>')
                in_table = False
            result_lines.append(line)
    if in_table:
        result_lines.append('</table>')
    text = '\n'.join(result_lines)

    # Paragraphs - convert remaining newlines
    text = re.sub(r'\n\n+', '</p><p>', text)
    text = text.replace('\n', '<br>')

    return text


def create_app(runtime: Runtime | None = None) -> FastAPI:
    """Create the FastAPI application.

    Args:
        runtime: Optional runtime instance to use

    Returns:
        Configured FastAPI application
    """
    global _runtime
    _runtime = runtime

    # Initialize message embedder for async embedding computation
    if runtime and hasattr(runtime, '_db') and hasattr(runtime, 'vector_store'):
        if runtime._db and runtime.vector_store:
            embedder = MessageEmbedder(runtime._db, runtime.vector_store)
            set_message_embedder(embedder)
            logger.info("Message embedder initialized for async embedding computation")

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

    # Set shared dependencies for route modules
    set_runtime(runtime)
    if templates:
        set_templates(templates)

    # Include route modules
    app.include_router(tasks_router)
    app.include_router(agents_router)
    app.include_router(memory_router)

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

    # Note: Agent page routes (/agents, /agent/*) are now in routes/agents.py
    # Note: /tasks route is now in routes/tasks.py
    # Note: /memory and /zettelkasten routes are now in routes/memory.py

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

    # Note: /task/{task_id} route is now in routes/tasks.py

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

    # Note: Agent API routes (/api/agents/*) are now in routes/agents.py

    @app.get("/api/budget")
    async def get_budget():
        """Get budget status."""
        if not _runtime:
            return {"error": "Runtime not available"}

        return await _runtime.get_budget_summary()

    # Note: Task API routes (/api/tasks/*) are now in routes/tasks.py

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

    # Note: Memory API routes (/api/memory/*) are now in routes/memory.py

    # -------------------------
    # HTMX Partials
    # -------------------------

    # Note: /partials/agents is now in routes/agents.py

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

    # Note: Task partials (/partials/tasks/*, /partials/task/*) are now in routes/tasks.py
    # Note: Memory partials (/partials/memory/*) are now in routes/memory.py

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
        import json as json_module

        role = row["role"]
        content = row["content"]
        timestamp = row["timestamp"] or ""

        time_str = ""
        if timestamp:
            try:
                dt = datetime.fromisoformat(timestamp)
                time_str = dt.strftime("%H:%M")
            except:
                pass

        # Check if content is multipart (JSON with images)
        text_content = content
        image_indicators = ""
        try:
            parsed = json_module.loads(content)
            if isinstance(parsed, dict) and parsed.get("type") == "multipart":
                text_content = parsed.get("text", "")
                images = parsed.get("images", [])
                if images:
                    # Show collapsed image indicators
                    img_html = []
                    for i, img in enumerate(images[:5]):
                        img_html.append(f'''
                            <span class="image-indicator" onclick="toggleImagePreview(this)" data-image="{img}">
                                <span class="image-icon">&#128247;</span> Image {i+1}
                            </span>
                        ''')
                    image_indicators = '<div class="image-indicators">' + ''.join(img_html) + '</div>'
        except (json_module.JSONDecodeError, TypeError):
            pass  # Not JSON, use as plain text

        # Render markdown
        rendered_content = _render_markdown(text_content)

        # Hook messages are collapsible
        if role == "hook":
            # Extract hook name from content (e.g., "memory_recall: ...")
            hook_name = text_content.split(":")[0] if ":" in text_content else "hook"
            hook_detail = text_content[len(hook_name)+1:].strip() if ":" in text_content else text_content
            rendered_detail = _render_markdown(hook_detail)

            return f"""
                <details class="chat-message hook" data-timestamp="{timestamp}">
                    <summary class="hook-summary">
                        <span class="hook-icon">&#9881;</span>
                        <span class="hook-name">{html_module.escape(hook_name)}</span>
                        <span class="meta">{time_str}</span>
                    </summary>
                    <div class="hook-content">{rendered_detail}</div>
                </details>
            """
        else:
            return f"""
                <div class="chat-message {role}" data-timestamp="{timestamp}">
                    {image_indicators}
                    <div class="content markdown-content">{rendered_content}</div>
                    <div class="meta">{time_str}</div>
                </div>
            """

    @app.get("/partials/agent/{agent_id}/messages", response_class=HTMLResponse)
    async def agent_chat_messages_partial(agent_id: str, conversation_id: str = ""):
        """HTMX partial for agent chat messages."""
        if not _runtime or not _runtime._db:
            return HTMLResponse('<div class="empty-state">Chat not available</div>')

        # Get chat messages from database
        with _runtime._db.connect() as conn:
            if conversation_id:
                rows = conn.execute(
                    """
                    SELECT role, content, timestamp FROM agent_chat_messages
                    WHERE agent_id = ? AND conversation_id = ?
                    ORDER BY timestamp ASC
                    LIMIT 100
                    """,
                    (agent_id, conversation_id),
                ).fetchall()
            else:
                # Legacy: get messages without conversation_id
                rows = conn.execute(
                    """
                    SELECT role, content, timestamp FROM agent_chat_messages
                    WHERE agent_id = ? AND conversation_id IS NULL
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
    async def agent_chat_messages_new_partial(agent_id: str, after: str = "", conversation_id: str = ""):
        """HTMX partial for new agent chat messages (only messages after timestamp)."""
        if not _runtime or not _runtime._db:
            return HTMLResponse("")

        with _runtime._db.connect() as conn:
            if after:
                # Get only new messages after the timestamp
                if conversation_id:
                    rows = conn.execute(
                        """
                        SELECT role, content, timestamp FROM agent_chat_messages
                        WHERE agent_id = ? AND conversation_id = ? AND timestamp > ?
                        ORDER BY timestamp ASC
                        LIMIT 50
                        """,
                        (agent_id, conversation_id, after),
                    ).fetchall()
                else:
                    rows = conn.execute(
                        """
                        SELECT role, content, timestamp FROM agent_chat_messages
                        WHERE agent_id = ? AND conversation_id IS NULL AND timestamp > ?
                        ORDER BY timestamp ASC
                        LIMIT 50
                        """,
                        (agent_id, after),
                    ).fetchall()
            else:
                # No timestamp - get recent messages (fallback for first poll)
                if conversation_id:
                    rows = conn.execute(
                        """
                        SELECT role, content, timestamp FROM agent_chat_messages
                        WHERE agent_id = ? AND conversation_id = ?
                        ORDER BY timestamp DESC
                        LIMIT 20
                        """,
                        (agent_id, conversation_id),
                    ).fetchall()
                else:
                    rows = conn.execute(
                        """
                        SELECT role, content, timestamp FROM agent_chat_messages
                        WHERE agent_id = ? AND conversation_id IS NULL
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
    async def agent_chat_send(
        agent_id: str,
        message: str = Form(...),
        conversation_id: str = Form(None),
        images: str = Form(None),  # JSON array of base64 data URLs
    ):
        """Send a message to an agent and get a response."""
        import json as json_module

        if not _runtime:
            raise HTTPException(status_code=503, detail="Runtime not available")

        agent = await _runtime.get_agent(agent_id)
        if not agent:
            raise HTTPException(status_code=404, detail=f"Agent not found: {agent_id}")

        if agent.status != "running":
            raise HTTPException(status_code=400, detail="Agent is not running")

        # Parse images if provided
        image_list = []
        if images:
            try:
                image_list = json_module.loads(images)
                if not isinstance(image_list, list):
                    image_list = []
            except:
                image_list = []

        # Build content - either plain text or multipart JSON
        if image_list:
            content = json_module.dumps({
                "type": "multipart",
                "text": message,
                "images": image_list[:5],  # Max 5 images
            })
        else:
            content = message

        # Store user message with token count for context management
        user_token_count = count_tokens(content)
        user_message_id = None
        with _runtime._db.connect() as conn:
            cursor = conn.execute(
                """INSERT INTO agent_chat_messages
                   (agent_id, role, content, conversation_id, token_count)
                   VALUES (?, ?, ?, ?, ?)""",
                (agent_id, "user", content, conversation_id, user_token_count),
            )
            user_message_id = cursor.lastrowid

            # Update conversation title if this is the first message
            if conversation_id:
                # Check if conversation has a title
                row = conn.execute(
                    "SELECT title FROM conversations WHERE id = ?",
                    (conversation_id,),
                ).fetchone()
                if row and not row["title"]:
                    # Set title from first user message (truncate to 40 chars)
                    title = message[:40] if len(message) <= 40 else message[:37] + "..."
                    conn.execute(
                        """UPDATE conversations
                           SET title = ?, updated_at = CURRENT_TIMESTAMP
                           WHERE id = ?""",
                        (title, conversation_id),
                    )
                else:
                    # Update conversation timestamp
                    conn.execute(
                        "UPDATE conversations SET updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                        (conversation_id,),
                    )

        # Schedule async embedding computation for user message (non-blocking)
        embedder = get_message_embedder()
        if embedder and user_message_id:
            embedder.schedule_embedding(user_message_id, content)

        # Get the backend for this agent
        backend = _runtime._backends.get(agent_id)
        if not backend:
            raise HTTPException(status_code=500, detail="Agent backend not available")

        # Load conversation history using dynamic context management
        # This maximizes context utilization while staying within token limits
        # See ADR-014 for the algorithm details
        if conversation_id and hasattr(backend, "set_history"):
            with _runtime._db.connect() as conn:
                # Get all messages with token counts
                # Note: agent responses are stored as 'agent' role, not 'assistant'
                history_rows = conn.execute(
                    """SELECT id, role, content, token_count, embedding, timestamp
                       FROM agent_chat_messages
                       WHERE agent_id = ? AND conversation_id = ?
                       AND role IN ('user', 'agent')
                       ORDER BY timestamp""",
                    (agent_id, conversation_id),
                ).fetchall()

                if history_rows:
                    # Convert to Message objects for context management
                    messages = []
                    for row in history_rows:
                        token_count = row["token_count"] or count_tokens(row["content"])
                        messages.append(Message(
                            id=row["id"],
                            role="assistant" if row["role"] == "agent" else row["role"],
                            content=row["content"],
                            token_count=token_count,
                            embedding=row["embedding"],
                            timestamp=row["timestamp"],
                        ))

                    # Get model from backend for context limits
                    model = getattr(backend, "model", "claude-sonnet-4-20250514")

                    # Create context manager for this model
                    context_mgr = ContextManager.for_model(
                        model=model,
                        target_utilization=0.90,
                        recent_window_ratio=0.50,
                        min_threshold=0.3,
                    )

                    # Assemble context with relevance-based selection
                    assembled_history, total_tokens = context_mgr.assemble_context(messages)

                    # Filter out elision markers for backends that don't understand them
                    history = [
                        {"role": m["role"], "content": m["content"]}
                        for m in assembled_history
                        if m["content"] != "[...]"
                    ]

                    backend.set_history(history)
                    logger.debug(
                        f"Context management: {len(messages)} messages -> {len(history)} selected, "
                        f"{total_tokens} tokens (model: {model})"
                    )

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
                        "INSERT INTO agent_chat_messages (agent_id, role, content, conversation_id) VALUES (?, ?, ?, ?)",
                        (agent_id, "hook", "memory_recall:\n" + "\n".join(log_parts), conversation_id),
                    )
            except Exception as e:
                logger.error(f"Memory recall hook error: {e}")
                with _runtime._db.connect() as conn:
                    conn.execute(
                        "INSERT INTO agent_chat_messages (agent_id, role, content, conversation_id) VALUES (?, ?, ?, ?)",
                        (agent_id, "hook", f"memory_recall error: {e}", conversation_id),
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

            # Store agent response with token count for context management
            agent_token_count = count_tokens(response_text)
            agent_message_id = None
            with _runtime._db.connect() as conn:
                cursor = conn.execute(
                    """INSERT INTO agent_chat_messages
                       (agent_id, role, content, conversation_id, token_count)
                       VALUES (?, ?, ?, ?, ?)""",
                    (agent_id, "agent", response_text, conversation_id, agent_token_count),
                )
                agent_message_id = cursor.lastrowid

            # Schedule async embedding computation for agent response (non-blocking)
            if embedder and agent_message_id:
                embedder.schedule_embedding(agent_message_id, response_text)

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
                                "INSERT INTO agent_chat_messages (agent_id, role, content, conversation_id) VALUES (?, ?, ?, ?)",
                                (agent_id, "hook", hook_msg, conversation_id),
                            )
                    except Exception as e:
                        logger.error(f"Memory remember hook error: {e}")
                        with _runtime._db.connect() as conn:
                            conn.execute(
                                "INSERT INTO agent_chat_messages (agent_id, role, content, conversation_id) VALUES (?, ?, ?, ?)",
                                (agent_id, "hook", f"memory_remember error: {e}", conversation_id),
                            )

            return {"status": "ok", "response": response_text}

        except Exception as e:
            logger.error(f"Agent chat error: {e}")
            # Store error as system message
            with _runtime._db.connect() as conn:
                conn.execute(
                    "INSERT INTO agent_chat_messages (agent_id, role, content, conversation_id) VALUES (?, ?, ?, ?)",
                    (agent_id, "system", f"Error: {str(e)}", conversation_id),
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

    # -------------------------
    # Conversation Management
    # -------------------------

    @app.get("/api/agent/{agent_id}/conversations")
    async def list_conversations(agent_id: str):
        """List all conversations for an agent."""
        if not _runtime or not _runtime._db:
            return []

        with _runtime._db.connect() as conn:
            rows = conn.execute(
                """
                SELECT id, title, created_at, updated_at
                FROM conversations
                WHERE agent_id = ?
                ORDER BY updated_at DESC
                """,
                (agent_id,),
            ).fetchall()

        return [
            {
                "id": row["id"],
                "title": row["title"],
                "created_at": row["created_at"],
                "updated_at": row["updated_at"],
            }
            for row in rows
        ]

    @app.post("/api/agent/{agent_id}/conversations")
    async def create_conversation(agent_id: str):
        """Create a new conversation for an agent."""
        import uuid

        if not _runtime or not _runtime._db:
            raise HTTPException(status_code=503, detail="Runtime not available")

        conv_id = str(uuid.uuid4())

        with _runtime._db.connect() as conn:
            conn.execute(
                """
                INSERT INTO conversations (id, agent_id, title)
                VALUES (?, ?, ?)
                """,
                (conv_id, agent_id, None),
            )

        return {"status": "ok", "conversation_id": conv_id}

    @app.delete("/api/agent/{agent_id}/conversations/{conv_id}")
    async def delete_conversation(agent_id: str, conv_id: str):
        """Delete a conversation and its messages."""
        if not _runtime or not _runtime._db:
            raise HTTPException(status_code=503, detail="Runtime not available")

        with _runtime._db.connect() as conn:
            # Delete messages first
            conn.execute(
                "DELETE FROM agent_chat_messages WHERE conversation_id = ?",
                (conv_id,),
            )
            # Delete conversation
            conn.execute(
                "DELETE FROM conversations WHERE id = ? AND agent_id = ?",
                (conv_id, agent_id),
            )

        return {"status": "ok"}

    @app.get("/partials/agent/{agent_id}/conversations", response_class=HTMLResponse)
    async def conversations_sidebar_partial(agent_id: str, current: str = ""):
        """HTMX partial for conversation sidebar list."""
        if not _runtime or not _runtime._db:
            return HTMLResponse('<div class="empty-state">Not available</div>')

        with _runtime._db.connect() as conn:
            rows = conn.execute(
                """
                SELECT id, title, updated_at
                FROM conversations
                WHERE agent_id = ?
                ORDER BY updated_at DESC
                LIMIT 50
                """,
                (agent_id,),
            ).fetchall()

        if not rows:
            return HTMLResponse('<div class="empty-state">No conversations yet</div>')

        html_parts = []
        for row in rows:
            conv_id = row["id"]
            title = row["title"] or "New conversation"
            # Truncate long titles
            if len(title) > 35:
                title = title[:32] + "..."

            active_class = "active" if conv_id == current else ""
            updated = row["updated_at"] or ""
            time_str = ""
            if updated:
                try:
                    dt = datetime.fromisoformat(updated)
                    time_str = dt.strftime("%b %d")
                except:
                    pass

            html_parts.append(f"""
                <div class="conversation-item {active_class}"
                     data-conv-id="{conv_id}"
                     onclick="selectConversation('{conv_id}')">
                    <div class="conv-title">{title}</div>
                    <div class="conv-meta">{time_str}</div>
                </div>
            """)

        return HTMLResponse("".join(html_parts))

    # -------------------------
    # Agent Info Panel
    # -------------------------

    @app.get("/partials/agent/{agent_id}/info-panel", response_class=HTMLResponse)
    async def agent_info_panel_partial(agent_id: str):
        """HTMX partial for agent info panel (budget, memory slots, tasks)."""
        if not _runtime:
            return HTMLResponse('<div class="empty-state">Not available</div>')

        html_parts = []

        # Budget section
        try:
            budget = await _runtime.get_budget_summary(agent_id)
            daily_spent = budget.get("daily_spent", 0)
            daily_limit = budget.get("daily_limit", 0)
            total_spent = budget.get("total_spent", 0)
            is_paused = budget.get("is_paused", False)

            pct = (daily_spent / daily_limit * 100) if daily_limit > 0 else 0
            pct = min(pct, 100)

            paused_html = '<span class="badge paused">PAUSED</span>' if is_paused else ""

            html_parts.append(f"""
                <div class="info-section">
                    <div class="info-header">Budget {paused_html}</div>
                    <div class="budget-bar">
                        <div class="budget-fill" style="width: {pct:.0f}%"></div>
                    </div>
                    <div class="budget-text">${daily_spent:.2f} / ${daily_limit:.2f} daily</div>
                    <div class="budget-total">Total: ${total_spent:.2f}</div>
                </div>
            """)
        except Exception as e:
            html_parts.append(f'<div class="info-section error">Budget: {e}</div>')

        # Memory slots section
        try:
            if _runtime.memory_manager:
                slot_manager = _runtime.memory_manager.get_slot_manager(agent_id)
                slots = await slot_manager.get_slots()
                stats = await slot_manager.get_stats()

                slot_html = '<div class="memory-slots-grid">'
                for slot in slots:
                    if not slot.is_empty:
                        relevance = slot.relevance_score
                        summary = slot.memory.summary[:50] if slot.memory and slot.memory.summary else ""
                        slot_html += f"""
                            <div class="memory-slot occupied" title="{summary}">
                                <span class="slot-num">{slot.index + 1}</span>
                                <span class="slot-score">{relevance:.1f}</span>
                            </div>
                        """
                    else:
                        slot_html += f'<div class="memory-slot empty"><span class="slot-num">{slot.index + 1}</span></div>'
                slot_html += '</div>'

                occupied = stats.get("active_slots", 0)
                total = stats.get("total_slots", 8)
                html_parts.append(f"""
                    <div class="info-section">
                        <div class="info-header">Memory Slots ({occupied}/{total})</div>
                        {slot_html}
                    </div>
                """)
            else:
                html_parts.append("""
                    <div class="info-section">
                        <div class="info-header">Memory Slots</div>
                        <div class="empty-state">Not configured</div>
                    </div>
                """)
        except Exception as e:
            html_parts.append(f'<div class="info-section error">Memory: {e}</div>')

        # Tasks section
        try:
            if _runtime.task_board:
                tasks = await _runtime.task_board.get_tasks(agent_id=agent_id, limit=5)
                if tasks:
                    task_html = ""
                    for task in tasks:
                        status_class = task.status.value.replace("_", "-")
                        task_html += f"""
                            <a href="/tasks?task={task.id}" class="task-item status-{status_class}">
                                <span class="task-status-dot"></span>
                                <span class="task-title">{task.title[:40]}</span>
                            </a>
                        """
                    html_parts.append(f"""
                        <div class="info-section">
                            <div class="info-header">Recent Tasks</div>
                            <div class="task-list-mini">{task_html}</div>
                        </div>
                    """)
                else:
                    html_parts.append("""
                        <div class="info-section">
                            <div class="info-header">Recent Tasks</div>
                            <div class="empty-state">No tasks</div>
                        </div>
                    """)
        except Exception as e:
            html_parts.append(f'<div class="info-section error">Tasks: {e}</div>')

        return HTMLResponse("".join(html_parts))

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
