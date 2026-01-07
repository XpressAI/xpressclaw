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
@click.argument("agent")
def chat(agent: str) -> None:
    """Start an interactive chat session with an agent.

    Example: xpressai chat atlas
    """
    from xpressai.cli.chat_cmd import run_chat

    run_chat(agent)


@cli.command()
@click.option("--agent", "-a", help="Filter by agent")
@click.option("--limit", "-n", default=50, help="Number of events to show")
@click.option("--follow", "-f", is_flag=True, help="Follow activity in real-time")
def activity(agent: str | None, limit: int, follow: bool) -> None:
    """View activity logs.

    Shows system events like task completions, agent errors, etc.
    """
    from xpressai.cli.activity_cmd import run_activity

    run_activity(agent=agent, limit=limit, follow=follow)


@cli.command()
@click.option("--port", "-p", default=8935, help="Port to check")
@click.option("--host", default="127.0.0.1", help="Host to check")
@click.option("--open", "-o", "open_browser", is_flag=True, help="Open in browser")
def dashboard(port: int, host: str, open_browser: bool) -> None:
    """Open the web dashboard.

    The dashboard is served by 'xpressai up'. This command checks if it's
    running and shows the URL.
    """
    import urllib.request

    url = f"http://{host}:{port}"

    # Check if runtime is serving the dashboard
    try:
        health_url = f"{url}/api/health"
        with urllib.request.urlopen(health_url, timeout=2) as response:
            # Runtime is running with dashboard
            click.echo(click.style("Dashboard available at:", fg="green"))
            click.echo(f"  {url}")
            click.echo()

            if open_browser:
                import webbrowser
                webbrowser.open(url)
                click.echo("Opened in browser.")
            else:
                click.echo("Run with --open to open in browser.")
            return
    except Exception:
        pass

    # No runtime running
    click.echo(click.style("Dashboard not available.", fg="yellow"))
    click.echo()
    click.echo("The dashboard is served by the runtime. Start it with:")
    click.echo(click.style("  xpressai up", fg="cyan"))
    click.echo()
    click.echo(f"Then open: {url}")


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


@tasks.command("assign")
@click.argument("task_id")
@click.argument("agent_id", required=False)
@click.pass_context
def tasks_assign(ctx: click.Context, task_id: str, agent_id: str | None) -> None:
    """Assign a task to an agent (or unassign if no agent given).

    Example: xpressai tasks atlas assign abc123 bob
    """
    from xpressai.cli.tasks_cmd import assign_task

    assign_task(task_id, agent_id)


@tasks.command("messages")
@click.argument("task_id")
@click.pass_context
def tasks_messages(ctx: click.Context, task_id: str) -> None:
    """Show conversation messages for a task.

    Example: xpressai tasks atlas messages abc123
    """
    from xpressai.cli.tasks_cmd import show_messages

    show_messages(task_id)


@tasks.command("message")
@click.argument("task_id")
@click.argument("content")
@click.pass_context
def tasks_message(ctx: click.Context, task_id: str, content: str) -> None:
    """Add a message to a task conversation.

    Example: xpressai tasks atlas message abc123 "Please also add tests"
    """
    from xpressai.cli.tasks_cmd import add_message

    add_message(task_id, content)


@tasks.command("retry")
@click.argument("task_id")
@click.pass_context
def tasks_retry(ctx: click.Context, task_id: str) -> None:
    """Retry a failed task from scratch.

    Clears conversation history and resets to pending.
    """
    from xpressai.cli.tasks_cmd import retry_task

    retry_task(task_id)


@tasks.command("schedule")
@click.argument("title")
@click.option(
    "--cron", "-c", required=True, help="Cron expression (e.g., '0 9 * * *' for 9am daily)"
)
@click.option("--name", "-n", help="Name for this schedule")
@click.pass_context
def tasks_schedule(ctx: click.Context, title: str, cron: str, name: str | None) -> None:
    """Schedule a recurring task.

    Examples:
        xpressai tasks atlas schedule "Summarize HN" --cron "0 9 * * *"
        xpressai tasks atlas schedule "Weekly report" --cron "0 17 * * 5" --name weekly-report
    """
    from xpressai.cli.tasks_cmd import schedule_task

    schedule_task(title=title, agent=ctx.obj["agent"], cron=cron, name=name)


@tasks.command("schedules")
@click.pass_context
def tasks_schedules(ctx: click.Context) -> None:
    """List scheduled tasks for this agent."""
    from xpressai.cli.tasks_cmd import list_schedules

    list_schedules(agent=ctx.obj["agent"])


@tasks.command("unschedule")
@click.argument("schedule_id")
def tasks_unschedule(schedule_id: str) -> None:
    """Remove a scheduled task."""
    from xpressai.cli.tasks_cmd import remove_schedule

    remove_schedule(schedule_id)


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


@cli.group()
def memory() -> None:
    """Inspect agent memory state."""
    pass


@memory.command("list")
@click.option("--agent", "-a", help="Filter by agent")
@click.option("--limit", "-n", default=10, help="Number of memories to show")
def memory_list(agent: str | None, limit: int) -> None:
    """List recent memories."""
    from xpressai.cli.memory_cmd import run_memory_list

    run_memory_list(agent=agent, limit=limit)


@memory.command("search")
@click.argument("query")
@click.option("--agent", "-a", help="Filter by agent")
@click.option("--limit", "-n", default=10, help="Maximum results")
def memory_search(query: str, agent: str | None, limit: int) -> None:
    """Search memories by semantic similarity.

    Example: xpressai memory search "deployment process"
    """
    from xpressai.cli.memory_cmd import run_memory_search

    run_memory_search(query=query, agent=agent, limit=limit)


@memory.command("show")
@click.argument("memory_id")
def memory_show(memory_id: str) -> None:
    """Show details of a specific memory."""
    from xpressai.cli.memory_cmd import run_memory_show

    run_memory_show(memory_id=memory_id)


@memory.command("stats")
def memory_stats() -> None:
    """Show memory system statistics."""
    from xpressai.cli.memory_cmd import run_memory_stats

    run_memory_stats()


@memory.command("slots")
@click.argument("agent")
def memory_slots(agent: str) -> None:
    """Show near-term memory slots for an agent.

    Example: xpressai memory slots atlas
    """
    from xpressai.cli.memory_cmd import run_memory_slots

    run_memory_slots(agent=agent)


@memory.command("delete")
@click.argument("memory_id")
def memory_delete(memory_id: str) -> None:
    """Delete a memory by ID.

    Example: xpressai memory delete abc123
    """
    from xpressai.cli.memory_cmd import run_memory_delete

    run_memory_delete(memory_id=memory_id)


if __name__ == "__main__":
    cli()
