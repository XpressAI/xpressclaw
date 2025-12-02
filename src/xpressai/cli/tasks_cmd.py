"""XpressAI tasks commands - Manage the task board."""

import asyncio
import click

from xpressai.core.runtime import get_runtime
from xpressai.tasks.board import TaskStatus


def list_tasks(agent: str, status: str | None = None) -> None:
    """List tasks for an agent."""
    asyncio.run(_list_tasks_async(agent, status))


async def _list_tasks_async(agent: str, status: str | None) -> None:
    """List tasks asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    if runtime._task_board is None:
        click.echo("Task board not initialized.")
        return

    # Get tasks for this agent
    status_filter = TaskStatus(status) if status else None
    tasks = await runtime._task_board.get_tasks(status=status_filter, agent_id=agent)

    click.echo(click.style(f"Tasks for @{agent}", fg="cyan", bold=True))
    click.echo()

    if not tasks:
        click.echo("No tasks.")
        click.echo(f'Add a task: xpressai tasks {agent} add "Task title"')
        return

    # Group by status
    by_status: dict[str, list] = {
        "pending": [],
        "in_progress": [],
        "completed": [],
        "blocked": [],
        "cancelled": [],
    }

    for task in tasks:
        by_status[task.status.value].append(task)

    # Show columns (only non-empty)
    columns = [
        ("Pending", "pending", "yellow"),
        ("In Progress", "in_progress", "blue"),
        ("Blocked", "blocked", "red"),
        ("Completed", "completed", "green"),
    ]

    for name, key, color in columns:
        task_list = by_status[key]
        if not task_list:
            continue
        if status is not None and status != key:
            continue

        click.echo(click.style(f"{name} ({len(task_list)})", fg=color, bold=True))
        for task in task_list:
            task_id = click.style(task.id[:8], dim=True)
            click.echo(f"  {task_id} {task.title}")
            if task.sop_id:
                click.echo(f"           SOP: {task.sop_id}")
        click.echo()

    # Group by status
    by_status: dict[str, list] = {
        "pending": [],
        "in_progress": [],
        "completed": [],
        "blocked": [],
        "cancelled": [],
    }

    for task in tasks:
        by_status[task.status.value].append(task)

    # Show columns
    columns = [
        ("Pending", "pending", "yellow"),
        ("In Progress", "in_progress", "blue"),
        ("Completed", "completed", "green"),
    ]

    if show_all:
        columns.extend(
            [
                ("Blocked", "blocked", "red"),
                ("Cancelled", "cancelled", "white"),
            ]
        )

    for name, key, color in columns:
        if status is None or status == key:
            task_list = by_status[key]
            click.echo(click.style(f"{name} ({len(task_list)})", fg=color, bold=True))
            for task in task_list:
                agent = (
                    click.style(f"@{task.agent_id}", fg="cyan")
                    if task.agent_id
                    else click.style("unassigned", fg="red")
                )
                task_id = click.style(task.id[:8], dim=True)
                click.echo(f"  {task_id} {agent} {task.title}")
                if task.sop_id:
                    click.echo(f"           SOP: {task.sop_id}")
                if task.description:
                    click.echo(f"           {task.description[:50]}...")
            click.echo()


def add_task(
    title: str,
    agent: str,
    sop: str | None = None,
    priority: int = 0,
) -> None:
    """Add a new task for an agent."""
    asyncio.run(_add_task_async(title, agent, sop, priority))


async def _add_task_async(
    title: str,
    agent: str,
    sop: str | None,
    priority: int,
) -> None:
    """Add task asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    if runtime._task_board is None:
        click.echo("Task board not initialized.")
        return

    # Verify agent exists
    agent_state = await runtime.get_agent(agent)
    if not agent_state:
        available = await runtime.list_agents()
        agent_names = [a.name for a in available]
        click.echo(click.style(f"Unknown agent: {agent}", fg="red"))
        if agent_names:
            click.echo(f"Available agents: {', '.join(agent_names)}")
        return

    task = await runtime._task_board.create_task(
        title=title,
        agent_id=agent,
        sop_id=sop,
        priority=priority,
    )

    click.echo(click.style(f"Created task: {task.title}", fg="green"))
    if sop:
        click.echo(f"  SOP: {sop}")
    click.echo(f"  ID: {task.id[:8]}")


def complete_task(task_id: str) -> None:
    """Mark a task as completed."""
    asyncio.run(_update_status_async(task_id, TaskStatus.COMPLETED))


def start_task(task_id: str, agent: str | None = None) -> None:
    """Mark a task as in progress."""
    asyncio.run(_update_status_async(task_id, TaskStatus.IN_PROGRESS, agent))


def block_task(task_id: str) -> None:
    """Mark a task as blocked."""
    asyncio.run(_update_status_async(task_id, TaskStatus.BLOCKED))


def cancel_task(task_id: str) -> None:
    """Cancel a task."""
    asyncio.run(_update_status_async(task_id, TaskStatus.CANCELLED))


async def _update_status_async(
    task_id: str,
    status: TaskStatus,
    agent: str | None = None,
) -> None:
    """Update task status asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    if runtime._task_board is None:
        click.echo("Task board not initialized.")
        return

    # Find task by ID prefix
    tasks = await runtime._task_board.get_tasks()
    matching = [t for t in tasks if t.id.startswith(task_id)]

    if not matching:
        click.echo(click.style(f"Task not found: {task_id}", fg="red"))
        return

    if len(matching) > 1:
        click.echo(click.style(f"Multiple tasks match '{task_id}'. Be more specific.", fg="red"))
        for t in matching:
            click.echo(f"  {t.id[:8]}... - {t.title}")
        return

    task = matching[0]
    await runtime._task_board.update_status(task.id, status, agent)

    status_colors = {
        TaskStatus.COMPLETED: "green",
        TaskStatus.IN_PROGRESS: "blue",
        TaskStatus.BLOCKED: "red",
        TaskStatus.CANCELLED: "white",
    }

    click.echo(
        click.style(
            f"Task '{task.title}' marked as {status.value}",
            fg=status_colors.get(status, "white"),
        )
    )


def delete_task(task_id: str) -> None:
    """Delete a task."""
    asyncio.run(_delete_task_async(task_id))


async def _delete_task_async(task_id: str) -> None:
    """Delete task asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    if runtime._task_board is None:
        click.echo("Task board not initialized.")
        return

    # Find task by ID prefix
    tasks = await runtime._task_board.get_tasks()
    matching = [t for t in tasks if t.id.startswith(task_id)]

    if not matching:
        click.echo(click.style(f"Task not found: {task_id}", fg="red"))
        return

    if len(matching) > 1:
        click.echo(click.style(f"Multiple tasks match '{task_id}'. Be more specific.", fg="red"))
        for t in matching:
            click.echo(f"  {t.id[:8]}... - {t.title}")
        return

    task = matching[0]
    await runtime._task_board.delete_task(task.id)

    click.echo(click.style(f"Deleted task: {task.title}", fg="green"))
