"""XpressAI tasks commands - Manage the task board."""

import asyncio
import click

from xpressai.core.runtime import get_runtime


def list_tasks(status: str | None = None) -> None:
    """List tasks on the board."""
    asyncio.run(_list_tasks_async(status))


async def _list_tasks_async(status: str | None) -> None:
    """List tasks asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    # Get task counts
    counts = await runtime.get_task_counts()

    click.echo(click.style("Task Board", fg="cyan", bold=True))
    click.echo()

    # Show columns
    columns = [
        ("Pending", "pending", "yellow"),
        ("In Progress", "in_progress", "blue"),
        ("Completed", "completed", "green"),
    ]

    for name, key, color in columns:
        if status is None or status == key:
            count = counts.get(key, 0)
            click.echo(click.style(f"{name}: {count}", fg=color, bold=True))

    click.echo()
    click.echo("Use 'xpressai tui' for full task board interface")
