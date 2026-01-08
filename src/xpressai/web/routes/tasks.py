"""Task management routes.

Contains API routes and HTMX partials for task operations.
"""

from __future__ import annotations

import html as html_module
import logging
from typing import Optional

from fastapi import APIRouter, Request, HTTPException, Form
from fastapi.responses import HTMLResponse

from xpressai.web.deps import get_runtime, get_templates, render_markdown

logger = logging.getLogger(__name__)

router = APIRouter()


# -------------------------
# Page Routes
# -------------------------

@router.get("/tasks", response_class=HTMLResponse)
async def tasks_page(request: Request):
    """Tasks page."""
    runtime = get_runtime()
    templates = get_templates()

    agents = []
    if runtime:
        agents = await runtime.list_agents()

    if templates:
        return templates.TemplateResponse(
            "tasks.html", {"request": request, "active": "tasks", "agents": agents}
        )
    return HTMLResponse("<h1>Tasks - Templates not installed</h1>")


@router.get("/task/{task_id}", response_class=HTMLResponse)
async def task_detail_page(request: Request, task_id: str):
    """Task detail page with conversation thread."""
    runtime = get_runtime()
    templates = get_templates()

    if not runtime or not runtime.task_board:
        return HTMLResponse("<h1>Runtime not available</h1>")

    try:
        task = await runtime.task_board.get_task(task_id)
    except Exception:
        raise HTTPException(status_code=404, detail="Task not found")

    # Get conversation messages
    messages = []
    if hasattr(runtime, 'conversation_manager') and runtime.conversation_manager:
        messages = await runtime.conversation_manager.get_messages(task_id)

    # Get agents for assignment dropdown
    agents = await runtime.list_agents()

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

@router.get("/api/tasks")
async def list_tasks():
    """List all tasks."""
    runtime = get_runtime()
    if not runtime:
        return {"tasks": []}

    counts = await runtime.get_task_counts()
    return {"counts": counts}


@router.post("/api/tasks")
async def create_task(
    title: str = Form(...),
    description: Optional[str] = Form(None),
    agent_id: Optional[str] = Form(None),
):
    """Create a new task from form data."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    # Convert empty string to None for agent_id
    if agent_id == "":
        agent_id = None

    task = await runtime.task_board.create_task(
        title=title,
        description=description if description else None,
        agent_id=agent_id,
    )

    return {
        "id": task.id,
        "title": task.title,
        "status": task.status.value,
    }


@router.get("/api/tasks/{task_id}")
async def get_task(task_id: str):
    """Get a specific task."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    try:
        task = await runtime.task_board.get_task(task_id)
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


@router.get("/api/tasks/{task_id}/messages")
async def get_task_messages(task_id: str):
    """Get messages for a task."""
    runtime = get_runtime()
    if not runtime:
        raise HTTPException(status_code=503, detail="Runtime not available")

    if not hasattr(runtime, 'conversation_manager') or not runtime.conversation_manager:
        return {"messages": []}

    messages = await runtime.conversation_manager.get_messages(task_id)
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


@router.post("/api/tasks/{task_id}/messages")
async def add_task_message(task_id: str, content: str = Form(...)):
    """Add a user message to a task.

    If the task is waiting, completed, or blocked, this will resume it.
    """
    runtime = get_runtime()
    if not runtime:
        raise HTTPException(status_code=503, detail="Runtime not available")

    if not hasattr(runtime, 'conversation_manager') or not runtime.conversation_manager:
        raise HTTPException(status_code=503, detail="Conversation manager not available")

    # Get current task status
    task = await runtime.task_board.get_task(task_id)

    from xpressai.tasks.board import TaskStatus

    # Add the user message
    await runtime.conversation_manager.add_message(task_id, "user", content)

    # Resume task if it's in a terminal or waiting state
    if task.status in (TaskStatus.WAITING_FOR_INPUT, TaskStatus.COMPLETED, TaskStatus.BLOCKED):
        await runtime.task_board.update_status(task_id, TaskStatus.PENDING)

    return {"status": "ok"}


@router.post("/api/tasks/{task_id}/complete")
async def complete_task_manual(task_id: str):
    """Manually mark a task as completed."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    from xpressai.tasks.board import TaskStatus

    try:
        task = await runtime.task_board.get_task(task_id)
    except Exception:
        raise HTTPException(status_code=404, detail="Task not found")

    # Add a message noting manual completion
    if hasattr(runtime, 'conversation_manager') and runtime.conversation_manager:
        await runtime.conversation_manager.add_message(
            task_id, "system", "Task manually marked as completed by user"
        )

    await runtime.task_board.update_status(task_id, TaskStatus.COMPLETED)
    return {"status": "ok", "task_id": task_id}


@router.post("/api/tasks/{task_id}/fail")
async def fail_task_manual(task_id: str):
    """Manually mark a task as failed/blocked."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    from xpressai.tasks.board import TaskStatus

    try:
        task = await runtime.task_board.get_task(task_id)
    except Exception:
        raise HTTPException(status_code=404, detail="Task not found")

    # Add a message noting manual cancellation
    if hasattr(runtime, 'conversation_manager') and runtime.conversation_manager:
        await runtime.conversation_manager.add_message(
            task_id, "system", "Task manually cancelled/failed by user"
        )

    await runtime.task_board.update_status(task_id, TaskStatus.BLOCKED)
    return {"status": "ok", "task_id": task_id}


@router.post("/api/tasks/{task_id}/retry")
async def retry_task(task_id: str):
    """Retry a failed task from scratch.

    Clears the conversation history and resets the task to pending.
    """
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    from xpressai.tasks.board import TaskStatus

    try:
        task = await runtime.task_board.get_task(task_id)
    except Exception:
        raise HTTPException(status_code=404, detail="Task not found")

    # Clear conversation history
    if hasattr(runtime, 'conversation_manager') and runtime.conversation_manager:
        await runtime.conversation_manager.clear_messages(task_id)

    # Reset task to pending
    await runtime.task_board.update_status(task_id, TaskStatus.PENDING)
    return {"status": "ok", "task_id": task_id}


@router.post("/api/tasks/{task_id}/assign")
async def assign_task(task_id: str, agent_id: str = Form("")):
    """Assign a task to an agent."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    try:
        task = await runtime.task_board.get_task(task_id)
    except Exception:
        raise HTTPException(status_code=404, detail="Task not found")

    # Empty string means unassigned
    assigned_agent = agent_id if agent_id else None

    await runtime.task_board.assign_task(task_id, assigned_agent)
    return {"status": "ok", "task_id": task_id, "agent_id": assigned_agent}


@router.delete("/api/tasks/{task_id}")
async def delete_task(task_id: str):
    """Delete a specific task."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    try:
        await runtime.task_board.delete_task(task_id)
    except Exception as e:
        raise HTTPException(status_code=404, detail=f"Task not found or error: {e}")

    return {"status": "ok", "task_id": task_id}


@router.patch("/api/tasks/{task_id}")
async def update_task(task_id: str, request: Request):
    """Update a task's title and/or description."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    try:
        form = await request.form()
        title = form.get("title")
        description = form.get("description")

        # Convert empty strings to None for description (to allow clearing)
        if description == "":
            description = None

        task = await runtime.task_board.update_task(
            task_id,
            title=title if title else None,
            description=description,
        )
        return {"status": "ok", "task_id": task.id, "title": task.title}
    except Exception as e:
        raise HTTPException(status_code=404, detail=f"Task not found or error: {e}")


@router.patch("/api/tasks/{task_id}/status")
async def update_task_status(task_id: str, status: str = Form(...)):
    """Update a task's status (for drag and drop)."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    from xpressai.tasks.board import TaskStatus

    # Map status strings to TaskStatus enum
    status_map = {
        "pending": TaskStatus.PENDING,
        "in_progress": TaskStatus.IN_PROGRESS,
        "waiting_for_input": TaskStatus.WAITING_FOR_INPUT,
        "completed": TaskStatus.COMPLETED,
        "blocked": TaskStatus.BLOCKED,
    }

    if status not in status_map:
        raise HTTPException(status_code=400, detail=f"Invalid status: {status}")

    try:
        task = await runtime.task_board.update_status(task_id, status_map[status])
        return {"status": "ok", "task_id": task.id, "new_status": task.status.value}
    except Exception as e:
        raise HTTPException(status_code=404, detail=f"Task not found or error: {e}")


@router.delete("/api/tasks/completed/clear")
async def clear_completed_tasks():
    """Delete all completed tasks."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    from xpressai.tasks.board import TaskStatus

    count = await runtime.task_board.delete_tasks_by_status(TaskStatus.COMPLETED)
    return {"status": "ok", "deleted_count": count}


@router.delete("/api/tasks/blocked/clear")
async def clear_blocked_tasks():
    """Delete all blocked/failed tasks."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        raise HTTPException(status_code=503, detail="Runtime not available")

    from xpressai.tasks.board import TaskStatus

    count = await runtime.task_board.delete_tasks_by_status(TaskStatus.BLOCKED)
    return {"status": "ok", "deleted_count": count}


# -------------------------
# HTMX Partials
# -------------------------

@router.get("/partials/tasks", response_class=HTMLResponse)
async def tasks_partial(request: Request):
    """HTMX partial for tasks summary."""
    runtime = get_runtime()
    if not runtime:
        return HTMLResponse('<div class="empty-state">Tasks not available</div>')

    counts = await runtime.get_task_counts()
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


@router.get("/partials/tasks/done", response_class=HTMLResponse)
async def tasks_done_partial(request: Request):
    """HTMX partial for done tasks (completed + blocked/failed)."""
    runtime = get_runtime()
    if not runtime or not runtime.task_board:
        return HTMLResponse('<div class="empty-state">No tasks</div><span id="done-count" class="task-count" hx-swap-oob="true">0</span>')

    from xpressai.tasks.board import TaskStatus

    # Get both completed and blocked tasks
    completed_tasks = await runtime.task_board.get_tasks(status=TaskStatus.COMPLETED, limit=20)
    blocked_tasks = await runtime.task_board.get_tasks(status=TaskStatus.BLOCKED, limit=20)

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
            <a href="/task/{task.id}" class="task-card {status_class} {failed_class}"
               draggable="true" data-task-id="{task.id}">
                <div class="title">{task.title}</div>
                <div class="meta">{task.agent_id or 'unassigned'}</div>
            </a>
        """)

    # Add out-of-band swap for the count
    html_parts.append(f'<span id="done-count" class="task-count" hx-swap-oob="true">{count}</span>')

    return HTMLResponse("".join(html_parts))


@router.get("/partials/tasks/{status}", response_class=HTMLResponse)
async def tasks_by_status_partial(request: Request, status: str):
    """HTMX partial for tasks by status (for kanban board)."""
    runtime = get_runtime()

    # Map status to count element ID
    count_id_map = {
        "pending": "pending-count",
        "in_progress": "in-progress-count",
        "waiting_for_input": "waiting-count",
    }
    count_id = count_id_map.get(status, f"{status}-count")

    if not runtime or not runtime.task_board:
        return HTMLResponse(f'<div class="empty-state">No tasks</div><span id="{count_id}" class="task-count" hx-swap-oob="true">0</span>')

    from xpressai.tasks.board import TaskStatus
    try:
        status_enum = TaskStatus(status)
    except ValueError:
        return HTMLResponse(f'<div class="empty-state">Invalid status: {status}</div>')

    tasks = await runtime.task_board.get_tasks(status=status_enum, limit=20)
    count = len(tasks)

    if not tasks:
        return HTMLResponse(f'<div class="empty-state">No tasks</div><span id="{count_id}" class="task-count" hx-swap-oob="true">{count}</span>')

    html_parts = []
    for task in tasks:
        status_class = f"status-{task.status.value}"
        html_parts.append(f"""
            <a href="/task/{task.id}" class="task-card {status_class}"
               draggable="true" data-task-id="{task.id}">
                <div class="title">{task.title}</div>
                <div class="meta">{task.agent_id or 'unassigned'}</div>
            </a>
        """)

    # Add out-of-band swap for the count
    html_parts.append(f'<span id="{count_id}" class="task-count" hx-swap-oob="true">{count}</span>')

    return HTMLResponse("".join(html_parts))


@router.get("/partials/task/{task_id}/messages", response_class=HTMLResponse)
async def task_messages_partial(request: Request, task_id: str):
    """HTMX partial for task conversation messages."""
    runtime = get_runtime()

    if not runtime:
        return HTMLResponse('<div class="empty-state">Runtime not available</div>')

    try:
        messages = []
        if hasattr(runtime, 'conversation_manager') and runtime.conversation_manager:
            messages = await runtime.conversation_manager.get_messages(task_id)

        if not messages:
            return HTMLResponse('<div class="empty-state">No messages yet</div>')

        html_parts = ['<div class="conversation">']
        for msg in messages:
            timestamp = msg.timestamp.strftime("%H:%M")
            content = msg.content
            rendered_content = render_markdown(content)

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
                rendered_detail = render_markdown(hook_detail)
                html_parts.append(f"""
                    <details class="chat-message hook" data-timestamp="{msg.timestamp.isoformat()}">
                        <summary class="hook-summary">
                            <span class="hook-icon">&#9881;</span>
                            <span class="hook-name">{html_module.escape(hook_name)}</span>
                            <span class="meta">{timestamp}</span>
                        </summary>
                        <div class="hook-content">{rendered_detail}</div>
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
                        <div class="message-content markdown-content">{rendered_content}</div>
                    </details>
                """)
            elif is_tool_call:
                html_parts.append(f"""
                    <div class="message message-agent">
                        <div class="message-header">
                            <span class="message-role">AGENT</span>
                            <span class="message-time">{timestamp}</span>
                        </div>
                        <div class="message-content markdown-content">{rendered_content}</div>
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
                        <div class="message-content markdown-content">{rendered_content}</div>
                    </div>
                """)

        html_parts.append("</div>")
        return HTMLResponse("".join(html_parts))

    except Exception as e:
        logger.error(f"Error loading task messages: {e}")
        return HTMLResponse(f'<div class="empty-state error">Error loading messages: {html_module.escape(str(e))}</div>')
