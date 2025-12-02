"""Textual-based Terminal UI for XpressAI.

Provides a rich terminal interface for:
- Monitoring agent status and logs
- Viewing budget usage
- Managing tasks
- Interactive chat with agents
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.core.runtime import XpressAIRuntime

try:
    from textual.app import App, ComposeResult
    from textual.widgets import Header, Footer, Static, Log, DataTable
    from textual.containers import Container, Horizontal, Vertical
    from textual.binding import Binding

    TEXTUAL_AVAILABLE = True
except ImportError:
    TEXTUAL_AVAILABLE = False

    # Stub classes for when textual isn't installed
    class App:  # type: ignore
        pass

    ComposeResult = None  # type: ignore


class AgentStatusWidget(Static):
    """Widget showing agent status."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._agents: dict = {}

    def update_agents(self, agents: dict) -> None:
        """Update displayed agent information."""
        self._agents = agents
        self._render_agents()

    def _render_agents(self) -> None:
        """Render agent status."""
        if not self._agents:
            self.update("No agents running")
            return

        lines = ["[bold]Agents[/bold]\n"]
        for name, info in self._agents.items():
            status = info.get("status", "unknown")
            status_color = {
                "running": "green",
                "stopped": "red",
                "starting": "yellow",
                "error": "red",
            }.get(status, "white")
            lines.append(f"  [{status_color}]{status}[/{status_color}] {name}")

        self.update("\n".join(lines))


class BudgetWidget(Static):
    """Widget showing budget usage."""

    def update_budget(self, daily_used: float, daily_limit: float) -> None:
        """Update budget display."""
        pct = (daily_used / daily_limit * 100) if daily_limit > 0 else 0
        bar_width = 20
        filled = int(bar_width * pct / 100)
        bar = "[green]" + "" * filled + "[/green]" + "" * (bar_width - filled)

        self.update(
            f"[bold]Budget[/bold]\n"
            f"  Daily: ${daily_used:.2f} / ${daily_limit:.2f}\n"
            f"  {bar} {pct:.1f}%"
        )


class LogWidget(Log):
    """Widget for displaying agent logs."""

    def add_log(self, agent: str, level: str, message: str) -> None:
        """Add a log entry."""
        level_color = {
            "INFO": "blue",
            "WARN": "yellow",
            "ERROR": "red",
            "DEBUG": "dim",
        }.get(level, "white")

        self.write_line(f"[{level_color}][{level}][/{level_color}] [{agent}] {message}")


if TEXTUAL_AVAILABLE:

    class XpressAIApp(App):
        """Main XpressAI Terminal UI Application."""

        TITLE = "XpressAI"
        SUB_TITLE = "Agent Runtime"

        CSS = """
        Screen {
            layout: grid;
            grid-size: 2 2;
            grid-columns: 1fr 2fr;
            grid-rows: auto 1fr;
        }
        
        #status-panel {
            row-span: 1;
            height: auto;
            min-height: 10;
            border: solid green;
            padding: 1;
        }
        
        #budget-panel {
            height: auto;
            min-height: 6;
            border: solid blue;
            padding: 1;
        }
        
        #log-panel {
            column-span: 2;
            border: solid white;
        }
        """

        BINDINGS = [
            Binding("q", "quit", "Quit"),
            Binding("r", "refresh", "Refresh"),
            Binding("l", "toggle_logs", "Logs"),
        ]

        def __init__(self, runtime: XpressAIRuntime | None = None):
            super().__init__()
            self.runtime = runtime

        def compose(self) -> ComposeResult:
            """Create child widgets."""
            yield Header()
            yield AgentStatusWidget(id="status-panel")
            yield BudgetWidget(id="budget-panel")
            yield LogWidget(id="log-panel")
            yield Footer()

        async def on_mount(self) -> None:
            """Called when app is mounted."""
            # Start refresh timer
            self.set_interval(2.0, self._refresh_status)
            await self._refresh_status()

        async def _refresh_status(self) -> None:
            """Refresh status displays."""
            if not self.runtime:
                return

            # Update agent status
            status_widget = self.query_one("#status-panel", AgentStatusWidget)
            agents = await self.runtime.get_agent_status()
            status_widget.update_agents(agents)

            # Update budget
            budget_widget = self.query_one("#budget-panel", BudgetWidget)
            if self.runtime.budget_manager:
                status = await self.runtime.budget_manager.get_status()
                budget_widget.update_budget(
                    status.get("daily_used", 0), status.get("daily_limit", 20.0)
                )

        def action_quit(self) -> None:
            """Quit the application."""
            self.exit()

        async def action_refresh(self) -> None:
            """Manual refresh."""
            await self._refresh_status()

        def action_toggle_logs(self) -> None:
            """Toggle log panel visibility."""
            log_panel = self.query_one("#log-panel")
            log_panel.display = not log_panel.display
else:

    class XpressAIApp:  # type: ignore
        """Placeholder when Textual is not available."""

        def __init__(self, runtime=None):
            self.runtime = runtime

        def run(self):
            print("Error: Textual is not installed. Install with: pip install textual")
            return 1


def run_tui(runtime: XpressAIRuntime | None = None) -> int:
    """Run the TUI application.

    Args:
        runtime: Optional runtime instance to monitor

    Returns:
        Exit code
    """
    if not TEXTUAL_AVAILABLE:
        print("Error: Textual is not installed.")
        print("Install with: pip install 'xpressai[tui]' or pip install textual")
        return 1

    app = XpressAIApp(runtime=runtime)
    app.run()
    return 0
