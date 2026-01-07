"""XpressAI activity command - View activity logs."""

import asyncio
from pathlib import Path
import click

from xpressai.core.config import load_config
from xpressai.core.runtime import Runtime


def run_activity(agent: str | None = None, limit: int = 50, follow: bool = False) -> None:
    """Show activity logs."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        click.echo("Run 'xpressai init' first.")
        return

    config = load_config(config_path)

    if follow:
        asyncio.run(_follow_activity(config, agent))
    else:
        asyncio.run(_show_activity(config, agent, limit))


async def _show_activity(config, agent: str | None, limit: int) -> None:
    """Show activity logs."""
    runtime = Runtime(config)
    await runtime.initialize()

    if not runtime.activity_manager:
        click.echo(click.style("Activity manager not available.", fg="yellow"))
        return

    if agent:
        events = await runtime.activity_manager.get_by_agent(agent, limit=limit)
    else:
        events = await runtime.activity_manager.get_recent(limit=limit)

    if not events:
        click.echo(click.style("No activity logs found.", fg="yellow"))
        return

    click.echo(click.style("Activity Log", fg="cyan", bold=True))
    click.echo()

    for event in events:
        _format_event(event)


async def _follow_activity(config, agent: str | None) -> None:
    """Follow activity logs in real-time."""
    import time

    runtime = Runtime(config)
    await runtime.initialize()

    if not runtime.activity_manager:
        click.echo(click.style("Activity manager not available.", fg="yellow"))
        return

    click.echo(click.style("Following activity... Press Ctrl+C to exit", fg="cyan"))
    click.echo()

    last_timestamp = None

    try:
        while True:
            if agent:
                events = await runtime.activity_manager.get_by_agent(agent, limit=10)
            else:
                events = await runtime.activity_manager.get_recent(limit=10)

            # Filter to only new events
            new_events = []
            for event in reversed(events):  # oldest first
                if last_timestamp is None or event.timestamp > last_timestamp:
                    new_events.append(event)
                    last_timestamp = event.timestamp

            for event in new_events:
                _format_event(event)

            await asyncio.sleep(1)
    except KeyboardInterrupt:
        pass


def _format_event(event) -> None:
    """Format and display an activity event."""
    timestamp = event.timestamp.strftime("%Y-%m-%d %H:%M:%S")
    event_type = event.event_type.value
    agent_id = event.agent_id or "system"

    # Determine color based on event type
    if "error" in event_type or "failed" in event_type:
        color = "red"
        icon = "✗"
    elif "started" in event_type:
        color = "blue"
        icon = "▶"
    elif "completed" in event_type:
        color = "green"
        icon = "✓"
    elif "waiting" in event_type:
        color = "yellow"
        icon = "⏸"
    else:
        color = "white"
        icon = "•"

    # Format event data
    data_parts = []
    if event.data:
        for key in ["title", "reason", "error", "summary"]:
            if key in event.data:
                value = str(event.data[key])[:60]
                data_parts.append(f"{key}={value}")

    data_str = " | ".join(data_parts) if data_parts else ""

    # Output
    click.echo(
        f"{click.style(timestamp, dim=True)} "
        f"{click.style(icon, fg=color)} "
        f"{click.style(f'[{agent_id}]', fg='cyan')} "
        f"{click.style(event_type, fg=color, bold=True)}"
    )
    if data_str:
        click.echo(f"    {data_str}")
