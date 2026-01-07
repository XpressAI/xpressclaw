"""XpressAI tasks commands - Manage the task board."""

import asyncio
import click

from xpressai.core.runtime import get_runtime
from xpressai.tasks.board import TaskStatus


def list_tasks(agent: str, status: str | None = None, show_all: bool = False) -> None:
    """List tasks for an agent."""
    asyncio.run(_list_tasks_async(agent, status, show_all))


async def _list_tasks_async(agent: str, status: str | None, show_all: bool = False) -> None:
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


def schedule_task(
    title: str,
    agent: str,
    cron: str,
    name: str | None = None,
) -> None:
    """Schedule a recurring task."""
    asyncio.run(_schedule_task_async(title, agent, cron, name))


async def _schedule_task_async(
    title: str,
    agent: str,
    cron: str,
    name: str | None,
) -> None:
    """Schedule task asynchronously."""
    import uuid

    runtime = get_runtime()
    await runtime.initialize()

    if runtime._scheduler is None:
        click.echo(click.style("Scheduler not initialized.", fg="red"))
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

    # Generate schedule ID and name
    schedule_id = str(uuid.uuid4())[:8]
    schedule_name = name or f"schedule-{schedule_id}"

    try:
        schedule = await runtime._scheduler.add_schedule(
            schedule_id=schedule_id,
            name=schedule_name,
            cron=cron,
            agent_id=agent,
            title=title,
        )

        next_run = runtime._scheduler.get_next_run(schedule_id)
        next_run_str = next_run.strftime("%Y-%m-%d %H:%M") if next_run else "unknown"

        click.echo(click.style(f"Scheduled: {schedule_name}", fg="green"))
        click.echo(f"  ID: {schedule_id}")
        click.echo(f"  Cron: {cron}")
        click.echo(f"  Agent: @{agent}")
        click.echo(f"  Task: {title}")
        click.echo(f"  Next run: {next_run_str}")

    except Exception as e:
        click.echo(click.style(f"Failed to create schedule: {e}", fg="red"))


def list_schedules(agent: str) -> None:
    """List scheduled tasks for an agent."""
    asyncio.run(_list_schedules_async(agent))


async def _list_schedules_async(agent: str) -> None:
    """List schedules asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    if runtime._scheduler is None:
        click.echo(click.style("Scheduler not initialized.", fg="red"))
        return

    # Start scheduler to load schedules from DB
    runtime._scheduler.start()

    schedules = runtime._scheduler.list_schedules()

    # Filter by agent
    agent_schedules = [s for s in schedules if s.agent_id == agent]

    click.echo(click.style(f"Scheduled tasks for @{agent}", fg="cyan", bold=True))
    click.echo()

    if not agent_schedules:
        click.echo("No scheduled tasks.")
        click.echo(f'Add one: xpressai tasks {agent} schedule "Task title" --cron "0 9 * * *"')
        return

    for schedule in agent_schedules:
        next_run = runtime._scheduler.get_next_run(schedule.id)
        next_run_str = next_run.strftime("%Y-%m-%d %H:%M") if next_run else "unknown"
        status = (
            click.style("[enabled]", fg="green")
            if schedule.enabled
            else click.style("[disabled]", fg="red")
        )

        click.echo(f"{status} {schedule.name}")
        click.echo(f"    ID: {schedule.id}")
        click.echo(f"    Cron: {schedule.cron}")
        click.echo(f"    Task: {schedule.title}")
        click.echo(f"    Next run: {next_run_str}")
        click.echo(f"    Run count: {schedule.run_count}")
        click.echo()


def remove_schedule(schedule_id: str) -> None:
    """Remove a scheduled task."""
    asyncio.run(_remove_schedule_async(schedule_id))


async def _remove_schedule_async(schedule_id: str) -> None:
    """Remove schedule asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    if runtime._scheduler is None:
        click.echo(click.style("Scheduler not initialized.", fg="red"))
        return

    # Start scheduler to load schedules from DB
    runtime._scheduler.start()

    # Find schedule by ID prefix
    schedules = runtime._scheduler.list_schedules()
    matching = [s for s in schedules if s.id.startswith(schedule_id)]

    if not matching:
        click.echo(click.style(f"Schedule not found: {schedule_id}", fg="red"))
        return

    if len(matching) > 1:
        click.echo(
            click.style(f"Multiple schedules match '{schedule_id}'. Be more specific.", fg="red")
        )
        for s in matching:
            click.echo(f"  {s.id} - {s.name}")
        return

    schedule = matching[0]
    await runtime._scheduler.remove_schedule(schedule.id)

    click.echo(click.style(f"Removed schedule: {schedule.name}", fg="green"))


def assign_task(task_id: str, agent_id: str | None) -> None:
    """Assign a task to an agent."""
    asyncio.run(_assign_task_async(task_id, agent_id))


async def _assign_task_async(task_id: str, agent_id: str | None) -> None:
    """Assign task asynchronously."""
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

    # Verify agent exists if assigning
    if agent_id:
        agent_state = await runtime.get_agent(agent_id)
        if not agent_state:
            available = await runtime.list_agents()
            agent_names = [a.name for a in available]
            click.echo(click.style(f"Unknown agent: {agent_id}", fg="red"))
            if agent_names:
                click.echo(f"Available agents: {', '.join(agent_names)}")
            return

    await runtime._task_board.assign_task(task.id, agent_id)

    if agent_id:
        click.echo(click.style(f"Task '{task.title}' assigned to @{agent_id}", fg="green"))
    else:
        click.echo(click.style(f"Task '{task.title}' unassigned", fg="yellow"))


def show_messages(task_id: str) -> None:
    """Show conversation messages for a task."""
    asyncio.run(_show_messages_async(task_id))


async def _show_messages_async(task_id: str) -> None:
    """Show task messages asynchronously."""
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

    # Get messages
    if not hasattr(runtime, 'conversation_manager') or not runtime.conversation_manager:
        click.echo(click.style("Conversation manager not available.", fg="yellow"))
        return

    messages = await runtime.conversation_manager.get_messages(task.id)

    click.echo(click.style(f"Conversation for: {task.title}", fg="cyan", bold=True))
    click.echo(click.style(f"Status: {task.status.value}", fg="white"))
    click.echo()

    if not messages:
        click.echo(click.style("No messages yet.", fg="yellow"))
        return

    role_colors = {
        "user": "green",
        "agent": "blue",
        "system": "white",
        "tool": "yellow",
        "hook": "cyan",
    }

    for msg in messages:
        color = role_colors.get(msg.role, "white")
        timestamp = msg.timestamp.strftime("%H:%M:%S")

        # Format header
        role_display = msg.role.upper()
        click.echo(click.style(f"[{timestamp}] {role_display}", fg=color, bold=True))

        # Format content (indent each line)
        for line in msg.content.split("\n"):
            click.echo(f"  {line}")
        click.echo()


def add_message(task_id: str, content: str) -> None:
    """Add a message to a task conversation."""
    asyncio.run(_add_message_async(task_id, content))


async def _add_message_async(task_id: str, content: str) -> None:
    """Add message asynchronously."""
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

    # Add message
    if not hasattr(runtime, 'conversation_manager') or not runtime.conversation_manager:
        click.echo(click.style("Conversation manager not available.", fg="yellow"))
        return

    await runtime.conversation_manager.add_message(task.id, "user", content)

    # Resume task if waiting
    if task.status == TaskStatus.WAITING_FOR_INPUT:
        await runtime._task_board.update_status(task.id, TaskStatus.PENDING)
        click.echo(click.style(f"Message added and task resumed.", fg="green"))
    else:
        click.echo(click.style(f"Message added to task.", fg="green"))


def retry_task(task_id: str) -> None:
    """Retry a failed task from scratch."""
    asyncio.run(_retry_task_async(task_id))


async def _retry_task_async(task_id: str) -> None:
    """Retry task asynchronously."""
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

    # Clear conversation history
    if hasattr(runtime, 'conversation_manager') and runtime.conversation_manager:
        await runtime.conversation_manager.clear_messages(task.id)

    # Reset task to pending
    await runtime._task_board.update_status(task.id, TaskStatus.PENDING)

    click.echo(click.style(f"Task '{task.title}' reset for retry.", fg="green"))
