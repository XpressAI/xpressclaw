"""XpressAI status command - Show agent status."""

import asyncio
from pathlib import Path
import click

from xpressai.core.config import load_config
from xpressai.core.runtime import Runtime


def run_status(watch: bool = False) -> None:
    """Show agent status, budget usage, and health."""
    config_path = Path.cwd() / "xpressai.yaml"
    
    if not config_path.exists():
        click.echo(click.style("⚠️  No xpressai.yaml found.", fg="yellow"))
        return
    
    config = load_config(config_path)
    
    if watch:
        _watch_status(config)
    else:
        asyncio.run(_show_status(config))


async def _show_status(config) -> None:
    """Show current status once."""
    runtime = Runtime(config)
    await runtime.initialize()
    
    agents = await runtime.list_agents()
    
    # Header
    click.echo(click.style("⚡ XpressAI Status", fg="cyan", bold=True))
    click.echo()
    
    # Agents
    click.echo(click.style("Agents:", bold=True))
    for agent in agents:
        status_icon = {
            "running": click.style("🟢", fg="green"),
            "stopped": "⚫",
            "error": click.style("🔴", fg="red"),
            "starting": click.style("🟡", fg="yellow"),
        }.get(agent.status, "❓")
        
        click.echo(f"  {status_icon} {agent.name}")
        click.echo(f"      Backend: {agent.backend}")
        click.echo(f"      Status:  {agent.status}")
        if agent.error_message:
            click.echo(click.style(f"      Error:   {agent.error_message}", fg="red"))
        click.echo()
    
    # Budget
    budget = await runtime.get_budget_summary()
    click.echo(click.style("Budget:", bold=True))
    if budget.get("limit"):
        pct = (budget.get("total_spent", 0) / budget["limit"]) * 100
        bar = _make_bar(pct)
        click.echo(f"  ${budget.get('total_spent', 0):.2f} / ${budget['limit']:.2f} ({pct:.0f}%) {bar}")
    else:
        click.echo(f"  ${budget.get('total_spent', 0):.2f} (no limit set)")
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
    
    bar = "█" * filled + "░" * empty
    return click.style(bar, fg=color)
