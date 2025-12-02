"""XpressAI status command - Show agent status."""

import asyncio
from pathlib import Path
import click

from xpressai.core.config import load_config
from xpressai.core.runtime import Runtime

# Default daemon API port
DAEMON_PORT = 8935


def run_status(watch: bool = False) -> None:
    """Show agent status, budget usage, and health."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        return

    config = load_config(config_path)

    if watch:
        _watch_status(config)
    else:
        # Try to get status from running daemon first
        daemon_status = _get_daemon_status()
        if daemon_status:
            _show_daemon_status(daemon_status)
        else:
            # No daemon running, show config-based status
            asyncio.run(_show_status(config))


async def _show_status(config) -> None:
    """Show current status once."""
    runtime = Runtime(config)
    await runtime.initialize()

    agents = await runtime.list_agents()

    # Header
    click.echo(click.style("XpressAI Status", fg="cyan", bold=True))
    click.echo()

    # Agents
    click.echo(click.style("Agents:", bold=True))
    for agent in agents:
        status_colors = {
            "running": "green",
            "stopped": "white",
            "error": "red",
            "starting": "yellow",
        }
        color = status_colors.get(agent.status, "white")

        click.echo(f"  [{agent.status}] {agent.name}")
        click.echo(f"      Backend: {agent.backend}")
        if agent.error_message:
            click.echo(click.style(f"      Error: {agent.error_message}", fg="red"))
        click.echo()

    # Budget
    budget = await runtime.get_budget_summary()
    click.echo(click.style("Budget:", bold=True))
    if budget.get("daily_limit"):
        daily_spent = budget.get("daily_spent", 0)
        daily_limit = budget["daily_limit"]
        pct = (daily_spent / daily_limit) * 100 if daily_limit else 0
        bar = _make_bar(pct)
        click.echo(f"  Daily: ${daily_spent:.2f} / ${daily_limit:.2f} ({pct:.0f}%) {bar}")
    else:
        click.echo(f"  Total: ${budget.get('total_spent', 0):.2f} (no limit set)")
    click.echo()

    # Tasks
    tasks = await runtime.get_task_counts()
    click.echo(click.style("Tasks:", bold=True))
    click.echo(f"  Pending:     {tasks.get('pending', 0)}")
    click.echo(f"  In Progress: {tasks.get('in_progress', 0)}")
    click.echo(f"  Completed:   {tasks.get('completed', 0)}")


def _watch_status(config) -> None:
    """Continuously watch status."""
    import time

    try:
        while True:
            # Clear screen
            click.clear()

            # Try daemon first
            daemon_status = _get_daemon_status()
            if daemon_status:
                _show_daemon_status(daemon_status)
            else:
                asyncio.run(_show_status(config))

            click.echo()
            click.echo(click.style("Watching... Press Ctrl+C to exit", fg="cyan"))
            time.sleep(2)
    except KeyboardInterrupt:
        pass


def _make_bar(percent: float, width: int = 10) -> str:
    """Make a progress bar."""
    filled = int(width * min(percent, 100) / 100)
    empty = width - filled

    if percent >= 90:
        color = "red"
    elif percent >= 70:
        color = "yellow"
    else:
        color = "green"

    bar = "#" * filled + "-" * empty
    return click.style(f"[{bar}]", fg=color)


def _get_daemon_status() -> dict | None:
    """Try to get status from running daemon.

    Returns:
        Status dict if daemon is running, None otherwise
    """
    import urllib.request
    import json

    try:
        url = f"http://127.0.0.1:{DAEMON_PORT}/api/status"
        with urllib.request.urlopen(url, timeout=1) as response:
            return json.loads(response.read().decode())
    except Exception:
        return None


def _show_daemon_status(status: dict) -> None:
    """Show status from running daemon."""
    # Header
    click.echo(click.style("XpressAI Status", fg="cyan", bold=True))
    click.echo(click.style("  (connected to running daemon)", fg="green"))
    click.echo()

    # Agents
    click.echo(click.style("Agents:", bold=True))
    agents = status.get("agents", [])
    if not agents:
        click.echo("  No agents configured")
    else:
        for agent in agents:
            agent_status = agent.get("status", "unknown")
            status_colors = {
                "running": "green",
                "stopped": "white",
                "error": "red",
                "starting": "yellow",
            }
            color = status_colors.get(agent_status, "white")

            click.echo(f"  [{agent_status}] {agent.get('name', 'unknown')}")
            click.echo(f"      Backend: {agent.get('backend', 'unknown')}")
    click.echo()

    # Budget
    budget = status.get("budget", {})
    click.echo(click.style("Budget:", bold=True))
    if budget.get("daily_limit"):
        daily_spent = float(budget.get("daily_spent", 0))
        daily_limit = float(budget["daily_limit"])
        pct = (daily_spent / daily_limit) * 100 if daily_limit else 0
        bar = _make_bar(pct)
        click.echo(f"  Daily: ${daily_spent:.2f} / ${daily_limit:.2f} ({pct:.0f}%) {bar}")
    else:
        click.echo(f"  Total: ${budget.get('total_spent', 0):.2f} (no limit set)")
    click.echo()

    # Tasks
    click.echo(click.style("Tasks:", bold=True))
    # Query tasks endpoint
    try:
        import urllib.request
        import json

        url = f"http://127.0.0.1:{DAEMON_PORT}/api/tasks"
        with urllib.request.urlopen(url, timeout=1) as response:
            tasks_data = json.loads(response.read().decode())
            counts = tasks_data.get("counts", {})
            click.echo(f"  Pending:     {counts.get('pending', 0)}")
            click.echo(f"  In Progress: {counts.get('in_progress', 0)}")
            click.echo(f"  Completed:   {counts.get('completed', 0)}")
    except Exception:
        click.echo("  (could not fetch task counts)")
