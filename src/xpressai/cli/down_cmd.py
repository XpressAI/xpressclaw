"""XpressAI down command - Stop all agents."""

import os
import signal
from pathlib import Path
import click


def _find_process_on_port(port: int) -> int | None:
    """Find PID of process LISTENING on a port.

    Returns:
        PID if found, None otherwise
    """
    import subprocess

    try:
        # Use lsof to find process listening on port (not just connected)
        result = subprocess.run(
            ["lsof", "-ti", f":{port}", "-sTCP:LISTEN"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0 and result.stdout.strip():
            # May return multiple PIDs, get first
            pids = result.stdout.strip().split("\n")
            return int(pids[0])
    except (subprocess.TimeoutExpired, FileNotFoundError, ValueError):
        pass

    return None


def run_down(agents: list[str] | None = None, timeout: int = 10) -> None:
    """Stop all agents gracefully."""
    pid_path = Path.home() / ".xpressai" / "runtime.pid"
    stopped_something = False

    # Try to stop by PID file first (daemonized mode)
    if pid_path.exists():
        pid = int(pid_path.read_text().strip())
        click.echo(click.style(f"Stopping runtime (PID {pid})...", fg="cyan"))

        try:
            os.kill(pid, signal.SIGTERM)
            stopped_something = True

            import time
            for i in range(timeout):
                try:
                    os.kill(pid, 0)
                    time.sleep(1)
                except OSError:
                    break
            else:
                click.echo(click.style("Force killing...", fg="yellow"))
                os.kill(pid, signal.SIGKILL)

            pid_path.unlink(missing_ok=True)
            click.echo(click.style("Stopped", fg="green"))

        except OSError as e:
            if e.errno == 3:  # No such process
                click.echo(click.style("Process not running.", fg="yellow"))
                pid_path.unlink(missing_ok=True)
            else:
                click.echo(click.style(f"Error: {e}", fg="red"))

    # Also check for orphaned API server on port 8935
    api_pid = _find_process_on_port(8935)
    if api_pid:
        click.echo(click.style(f"Found process on port 8935 (PID {api_pid})", fg="yellow"))

        # Check if it's a python/uvicorn process before killing
        try:
            import subprocess
            result = subprocess.run(
                ["ps", "-p", str(api_pid), "-o", "comm="],
                capture_output=True,
                text=True,
                timeout=5,
            )
            proc_name = result.stdout.strip().lower()

            if "python" in proc_name or "uvicorn" in proc_name:
                click.echo(f"Stopping orphaned API server...")
                os.kill(api_pid, signal.SIGTERM)
                stopped_something = True

                import time
                time.sleep(1)

                # Check if still running
                try:
                    os.kill(api_pid, 0)
                    os.kill(api_pid, signal.SIGKILL)
                except OSError:
                    pass

                click.echo(click.style("Stopped orphaned API server", fg="green"))
            else:
                click.echo(f"Port 8935 used by non-xpressai process: {proc_name}")
        except Exception as e:
            click.echo(click.style(f"Could not check process: {e}", fg="yellow"))

    if not stopped_something and not pid_path.exists():
        click.echo(click.style("No running runtime found.", fg="yellow"))
