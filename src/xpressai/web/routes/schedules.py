"""Schedule management routes.

Contains API routes and HTMX partials for scheduled tasks.
"""

from __future__ import annotations

import logging
import uuid
from datetime import datetime
from typing import Optional

from fastapi import APIRouter, Request, HTTPException, Form
from fastapi.responses import HTMLResponse

from xpressai.web.common import get_runtime

logger = logging.getLogger(__name__)

router = APIRouter()


# -------------------------
# Helper Functions
# -------------------------

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


# -------------------------
# API Routes
# -------------------------

@router.get("/api/schedules")
async def list_schedules():
    """List all scheduled tasks."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        return {"schedules": []}

    schedules = runtime._scheduler.list_schedules()
    result = []
    for s in schedules:
        next_run = runtime._scheduler.get_next_run(s.id)
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


@router.post("/api/schedules")
async def create_schedule(
    name: str = Form(...),
    cron: str = Form(...),
    agent_id: str = Form(...),
    title: str = Form(...),
    description: Optional[str] = Form(None),
):
    """Create a new scheduled task."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        raise HTTPException(status_code=503, detail="Scheduler not available")

    schedule_id = str(uuid.uuid4())[:8]

    try:
        schedule = await runtime._scheduler.add_schedule(
            schedule_id=schedule_id,
            name=name,
            cron=cron,
            agent_id=agent_id,
            title=title,
            description=description,
        )
        next_run = runtime._scheduler.get_next_run(schedule_id)
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


@router.delete("/api/schedules/{schedule_id}")
async def delete_schedule(schedule_id: str):
    """Delete a scheduled task."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        raise HTTPException(status_code=503, detail="Scheduler not available")

    # Find by prefix match
    schedules = runtime._scheduler.list_schedules()
    matching = [s for s in schedules if s.id.startswith(schedule_id)]

    if not matching:
        raise HTTPException(status_code=404, detail="Schedule not found")
    if len(matching) > 1:
        raise HTTPException(status_code=400, detail="Multiple schedules match, be more specific")

    await runtime._scheduler.remove_schedule(matching[0].id)
    return {"status": "ok"}


@router.post("/api/schedules/{schedule_id}/enable")
async def enable_schedule(schedule_id: str):
    """Enable a scheduled task."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        raise HTTPException(status_code=503, detail="Scheduler not available")

    success = await runtime._scheduler.enable_schedule(schedule_id)
    if not success:
        raise HTTPException(status_code=404, detail="Schedule not found")

    # Save to DB
    schedule = runtime._scheduler.get_schedule(schedule_id)
    if schedule:
        runtime._scheduler._save_schedule(schedule)

    return {"status": "ok"}


@router.post("/api/schedules/{schedule_id}/disable")
async def disable_schedule(schedule_id: str):
    """Disable a scheduled task."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        raise HTTPException(status_code=503, detail="Scheduler not available")

    success = await runtime._scheduler.disable_schedule(schedule_id)
    if not success:
        raise HTTPException(status_code=404, detail="Schedule not found")

    # Save to DB
    schedule = runtime._scheduler.get_schedule(schedule_id)
    if schedule:
        runtime._scheduler._save_schedule(schedule)

    return {"status": "ok"}


@router.post("/api/schedules/{schedule_id}/trigger")
async def trigger_schedule(schedule_id: str):
    """Manually trigger a scheduled task immediately."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        raise HTTPException(status_code=503, detail="Scheduler not available")

    task = await runtime._scheduler.trigger_now(schedule_id)
    if task is None:
        raise HTTPException(status_code=404, detail="Schedule not found")

    return {
        "status": "ok",
        "task": {
            "id": task.id,
            "title": task.title,
        }
    }


# -------------------------
# HTMX Partials
# -------------------------

@router.get("/partials/schedules/count", response_class=HTMLResponse)
async def schedules_count_partial(request: Request):
    """HTMX partial for active schedules count."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        return HTMLResponse("")

    schedules = runtime._scheduler.list_schedules()
    active = sum(1 for s in schedules if s.enabled)
    if not schedules:
        return HTMLResponse("")
    return HTMLResponse(f"({active} active)")


@router.get("/partials/schedules", response_class=HTMLResponse)
async def schedules_partial(request: Request):
    """HTMX partial for scheduled tasks list."""
    runtime = get_runtime()
    if not runtime or not runtime._scheduler:
        return HTMLResponse('<div class="no-schedules">Scheduler not available</div>')

    schedules = runtime._scheduler.list_schedules()
    if not schedules:
        return HTMLResponse('<div class="no-schedules">No scheduled tasks. Click "+ New Schedule" to create one.</div>')

    html_parts = []
    for s in schedules:
        next_run = runtime._scheduler.get_next_run(s.id)
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
                                {'<button class="dropdown-item" hx-post="/api/schedules/' + s.id + '/disable" hx-swap="none" hx-on::after-request="htmx.trigger(document.body, ' + "'" + 'scheduleUpdate' + "'" + ')">Pause</button>' if s.enabled else '<button class="dropdown-item" hx-post="/api/schedules/' + s.id + '/enable" hx-swap="none" hx-on::after-request="htmx.trigger(document.body, ' + "'" + 'scheduleUpdate' + "'" + ')">Resume</button>'}
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
