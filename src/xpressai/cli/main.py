"""XpressAI CLI - Main entry point."""

import click
from pathlib import Path

from xpressai import __version__


@click.group()
@click.version_option(version=__version__)
@click.pass_context
def cli(ctx: click.Context) -> None:
    """XpressAI - The Operating System for AI Agents.

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
    from xpressai.cli.init_cmd import run_init

    run_init(backend=backend, force=force)


@cli.command()
@click.option("--agent", "-a", multiple=True, help="Specific agents to start")
@click.option("--detach", "-d", is_flag=True, help="Run in background")
def up(agent: tuple[str, ...], detach: bool) -> None:
    """Start the runtime and agents.

    Launches containers and begins agent execution.
    """
    from xpressai.cli.up_cmd import run_up

    run_up(agents=list(agent), detach=detach)


@cli.command()
@click.option("--agent", "-a", multiple=True, help="Specific agents to stop")
@click.option("--timeout", default=10, help="Shutdown timeout in seconds")
def down(agent: tuple[str, ...], timeout: int) -> None:
    """Stop all agents gracefully."""
    from xpressai.cli.down_cmd import run_down

    run_down(agents=list(agent), timeout=timeout)


@cli.command()
@click.option("--watch", "-w", is_flag=True, help="Continuously update")
def status(watch: bool) -> None:
    """Show agent status, budget usage, and health."""
    from xpressai.cli.status_cmd import run_status

    run_status(watch=watch)


@cli.command()
@click.argument("agent", required=False)
@click.option("--follow", "-f", is_flag=True, help="Follow log output")
@click.option("--tail", "-n", default=100, help="Number of lines to show")
def logs(agent: str | None, follow: bool, tail: int) -> None:
    """Stream agent logs."""
    from xpressai.cli.logs_cmd import run_logs

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
@click.argument("agent")
@click.pass_context
def tasks(ctx: click.Context, agent: str) -> None:
    """Manage tasks for an agent.

    Example: xpressai tasks atlas list
    """
    ctx.ensure_object(dict)
    ctx.obj["agent"] = agent


@tasks.command("list")
@click.option(
    "--status",
    "-s",
    type=click.Choice(["pending", "in_progress", "completed", "blocked", "cancelled"]),
)
@click.pass_context
def tasks_list(ctx: click.Context, status: str | None) -> None:
    """List tasks for this agent."""
    from xpressai.cli.tasks_cmd import list_tasks

    list_tasks(agent=ctx.obj["agent"], status=status)


@tasks.command("add")
@click.argument("title")
@click.option("--sop", "-s", help="SOP to follow for this task")
@click.option("--priority", "-p", default=0, help="Priority (higher = more important)")
@click.pass_context
def tasks_add(ctx: click.Context, title: str, sop: str | None, priority: int) -> None:
    """Add a new task.

    Example: xpressai tasks atlas add "Deploy to production" --sop deployment
    """
    from xpressai.cli.tasks_cmd import add_task

    add_task(title=title, agent=ctx.obj["agent"], sop=sop, priority=priority)


@tasks.command("complete")
@click.argument("task_id")
@click.pass_context
def tasks_complete(ctx: click.Context, task_id: str) -> None:
    """Mark a task as completed."""
    from xpressai.cli.tasks_cmd import complete_task

    complete_task(task_id)


@tasks.command("start")
@click.argument("task_id")
@click.pass_context
def tasks_start(ctx: click.Context, task_id: str) -> None:
    """Start working on a task."""
    from xpressai.cli.tasks_cmd import start_task

    start_task(task_id, ctx.obj["agent"])


@tasks.command("block")
@click.argument("task_id")
def tasks_block(task_id: str) -> None:
    """Mark a task as blocked."""
    from xpressai.cli.tasks_cmd import block_task

    block_task(task_id)


@tasks.command("cancel")
@click.argument("task_id")
def tasks_cancel(task_id: str) -> None:
    """Cancel a task."""
    from xpressai.cli.tasks_cmd import cancel_task

    cancel_task(task_id)


@tasks.command("delete")
@click.argument("task_id")
def tasks_delete(task_id: str) -> None:
    """Delete a task."""
    from xpressai.cli.tasks_cmd import delete_task

    delete_task(task_id)


@cli.group()
def sop() -> None:
    """Manage Standard Operating Procedures."""
    pass


@sop.command("list")
def sop_list() -> None:
    """List all SOPs."""
    from xpressai.cli.sop_cmd import list_sops

    list_sops()


@sop.command("show")
@click.argument("name")
def sop_show(name: str) -> None:
    """Show details of an SOP."""
    from xpressai.cli.sop_cmd import show_sop

    show_sop(name)


@sop.command("create")
@click.argument("name")
def sop_create(name: str) -> None:
    """Create a new SOP from a template."""
    from xpressai.cli.sop_cmd import create_sop

    create_sop(name)


@sop.command("delete")
@click.argument("name")
def sop_delete(name: str) -> None:
    """Delete an SOP."""
    from xpressai.cli.sop_cmd import delete_sop

    delete_sop(name)


@cli.group()
def budget() -> None:
    """View and manage budgets."""
    pass


@budget.command("show")
@click.argument("agent", required=False)
def budget_show(agent: str | None) -> None:
    """Show budget status."""
    from xpressai.cli.budget_cmd import show_budget

    show_budget(agent=agent)


if __name__ == "__main__":
    cli()
