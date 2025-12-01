"""XpressAI CLI - Main entry point."""

import click
from pathlib import Path

from xpressai import __version__


@click.group()
@click.version_option(version=__version__)
@click.pass_context
def cli(ctx: click.Context) -> None:
    """XpressAI - The Phusion Passenger for AI Agents.
    
    Run, manage, and observe AI agents with ease.
    """
    ctx.ensure_object(dict)


@cli.command()
@click.option("--backend", default="claude-code", help="Default agent backend")
@click.option("--force", is_flag=True, help="Overwrite existing configuration")
def init(backend: str, force: bool) -> None:
    """Initialize a new XpressAI workspace.
    
    Creates xpressai.yaml with sensible defaults.
    """
    from xpressai.cli.init import run_init
    run_init(backend=backend, force=force)


@cli.command()
@click.option("--agent", "-a", multiple=True, help="Specific agents to start")
@click.option("--detach", "-d", is_flag=True, help="Run in background")
def up(agent: tuple[str, ...], detach: bool) -> None:
    """Start the runtime and agents.
    
    Launches containers and begins agent execution.
    """
    from xpressai.cli.up import run_up
    run_up(agents=list(agent), detach=detach)


@cli.command()
@click.option("--agent", "-a", multiple=True, help="Specific agents to stop")
@click.option("--timeout", default=10, help="Shutdown timeout in seconds")
def down(agent: tuple[str, ...], timeout: int) -> None:
    """Stop all agents gracefully."""
    from xpressai.cli.down import run_down
    run_down(agents=list(agent), timeout=timeout)


@cli.command()
@click.option("--watch", "-w", is_flag=True, help="Continuously update")
def status(watch: bool) -> None:
    """Show agent status, budget usage, and health."""
    from xpressai.cli.status import run_status
    run_status(watch=watch)


@cli.command()
@click.argument("agent", required=False)
@click.option("--follow", "-f", is_flag=True, help="Follow log output")
@click.option("--tail", "-n", default=100, help="Number of lines to show")
def logs(agent: str | None, follow: bool, tail: int) -> None:
    """Stream agent logs."""
    from xpressai.cli.logs import run_logs
    run_logs(agent=agent, follow=follow, tail=tail)


@cli.command()
def tui() -> None:
    """Launch the Terminal User Interface."""
    from xpressai.tui.app import run_tui
    run_tui()


@cli.command()
@click.option("--port", "-p", default=7777, help="Port to run on")
@click.option("--host", default="127.0.0.1", help="Host to bind to")
def dashboard(port: int, host: str) -> None:
    """Open the web dashboard."""
    from xpressai.web.app import run_dashboard
    run_dashboard(host=host, port=port)


@cli.group()
def tasks() -> None:
    """Manage tasks and SOPs."""
    pass


@tasks.command("list")
@click.option("--status", type=click.Choice(["pending", "in_progress", "completed"]))
def tasks_list(status: str | None) -> None:
    """List tasks on the board."""
    from xpressai.cli.tasks import list_tasks
    list_tasks(status=status)


@cli.group()
def budget() -> None:
    """View and manage budgets."""
    pass


@budget.command("show")
@click.argument("agent", required=False)
def budget_show(agent: str | None) -> None:
    """Show budget status."""
    from xpressai.cli.budget import show_budget
    show_budget(agent=agent)


if __name__ == "__main__":
    cli()
