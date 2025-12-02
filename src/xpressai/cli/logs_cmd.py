"""XpressAI logs command - Stream agent logs."""

from pathlib import Path
import click


def run_logs(agent: str | None = None, follow: bool = False, tail: int = 100) -> None:
    """Stream agent logs."""
    log_path = Path.home() / ".xpressai" / "runtime.log"

    if not log_path.exists():
        click.echo(click.style("No logs found.", fg="yellow"))
        return

    if follow:
        _follow_logs(log_path, agent)
    else:
        _show_logs(log_path, agent, tail)


def _show_logs(log_path: Path, agent: str | None, tail: int) -> None:
    """Show last N lines of logs."""
    with open(log_path) as f:
        lines = f.readlines()

    # Filter by agent if specified
    if agent:
        lines = [l for l in lines if f"[{agent}]" in l]

    # Show last N lines
    for line in lines[-tail:]:
        _format_log_line(line)


def _follow_logs(log_path: Path, agent: str | None) -> None:
    """Follow logs in real-time."""
    import time

    click.echo(click.style("Following logs... Press Ctrl+C to exit", fg="cyan"))
    click.echo()

    try:
        with open(log_path) as f:
            # Go to end of file
            f.seek(0, 2)

            while True:
                line = f.readline()
                if line:
                    if agent is None or f"[{agent}]" in line:
                        _format_log_line(line)
                else:
                    time.sleep(0.1)
    except KeyboardInterrupt:
        pass


def _format_log_line(line: str) -> None:
    """Format and print a log line."""
    line = line.rstrip()

    if "[ERROR]" in line:
        click.echo(click.style(line, fg="red"))
    elif "[WARN]" in line:
        click.echo(click.style(line, fg="yellow"))
    elif "[INFO]" in line:
        click.echo(line)
    else:
        click.echo(click.style(line, fg="bright_black"))
