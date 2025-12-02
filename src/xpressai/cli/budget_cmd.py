"""XpressAI budget commands - View and manage budgets."""

import asyncio
import click

from xpressai.core.runtime import get_runtime


def show_budget(agent: str | None = None) -> None:
    """Show budget status."""
    asyncio.run(_show_budget_async(agent))


async def _show_budget_async(agent: str | None) -> None:
    """Show budget status asynchronously."""
    runtime = get_runtime()
    await runtime.initialize()

    agents = await runtime.list_agents()

    click.echo(click.style("Budget Status", fg="cyan", bold=True))
    click.echo()

    for a in agents:
        if agent is not None and a.name != agent:
            continue

        click.echo(click.style(f"Agent: {a.name}", bold=True))

        # Get budget info
        summary = await runtime.get_budget_summary(a.id)

        daily_spent = summary.get("daily_spent", 0)
        daily_limit = summary.get("daily_limit", 10.0)
        total_spent = summary.get("total_spent", 0)

        if daily_limit:
            # Daily
            daily_pct = (daily_spent / daily_limit) * 100 if daily_limit else 0
            daily_bar = _make_bar(daily_pct)
            click.echo(
                f"  Daily:   ${daily_spent:.2f} / ${daily_limit:.2f} ({daily_pct:.0f}%) {daily_bar}"
            )

        click.echo(f"  Total:   ${total_spent:.2f}")

        if summary.get("is_paused"):
            click.echo(
                click.style(
                    f"  Status: PAUSED - {summary.get('pause_reason', 'Unknown')}", fg="red"
                )
            )

        click.echo()


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
