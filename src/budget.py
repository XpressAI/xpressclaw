"""XpressAI budget commands - View and manage budgets."""

import asyncio
import click


def show_budget(agent: str | None = None) -> None:
    """Show budget status."""
    asyncio.run(_show_budget_async(agent))


async def _show_budget_async(agent: str | None) -> None:
    """Show budget status asynchronously."""
    from xpressai.core.runtime import get_runtime
    
    runtime = get_runtime()
    await runtime.initialize()
    
    agents = await runtime.list_agents()
    
    click.echo(click.style("💰 Budget Status", fg="cyan", bold=True))
    click.echo()
    
    for a in agents:
        if agent is not None and a.name != agent:
            continue
        
        click.echo(click.style(f"Agent: {a.name}", bold=True))
        
        # Get budget info (placeholder)
        daily_spent = 0.00
        daily_limit = 10.00
        monthly_spent = 0.00
        monthly_limit = 100.00
        
        # Daily
        daily_pct = (daily_spent / daily_limit) * 100 if daily_limit else 0
        daily_bar = _make_bar(daily_pct)
        click.echo(f"  Daily:   ${daily_spent:.2f} / ${daily_limit:.2f} ({daily_pct:.0f}%) {daily_bar}")
        
        # Monthly
        monthly_pct = (monthly_spent / monthly_limit) * 100 if monthly_limit else 0
        monthly_bar = _make_bar(monthly_pct)
        click.echo(f"  Monthly: ${monthly_spent:.2f} / ${monthly_limit:.2f} ({monthly_pct:.0f}%) {monthly_bar}")
        
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
    
    bar = "█" * filled + "░" * empty
    return click.style(bar, fg=color)
