"""XpressAI down command - Stop all agents."""

import os
import signal
from pathlib import Path
import click


def run_down(agents: list[str] | None = None, timeout: int = 10) -> None:
    """Stop all agents gracefully."""
    pid_path = Path.home() / ".xpressai" / "runtime.pid"

    if not pid_path.exists():
        click.echo(click.style("No running runtime found.", fg="yellow"))
        return

    pid = int(pid_path.read_text().strip())

    click.echo(click.style(f"Stopping runtime (PID {pid})...", fg="cyan"))

    try:
        # Send SIGTERM
        os.kill(pid, signal.SIGTERM)

        # Wait for process to stop
        import time

        for i in range(timeout):
            try:
                os.kill(pid, 0)  # Check if process exists
                time.sleep(1)
            except OSError:
                # Process stopped
                break
        else:
            # Force kill if still running
            click.echo(click.style("Force killing...", fg="yellow"))
            os.kill(pid, signal.SIGKILL)

        # Clean up PID file
        pid_path.unlink(missing_ok=True)

        click.echo(click.style("Stopped", fg="green"))

    except OSError as e:
        if e.errno == 3:  # No such process
            click.echo(click.style("Process not running.", fg="yellow"))
            pid_path.unlink(missing_ok=True)
        else:
            click.echo(click.style(f"Error: {e}", fg="red"))
