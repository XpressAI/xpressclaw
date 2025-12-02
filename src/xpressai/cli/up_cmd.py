"""XpressAI up command - Start the runtime and agents."""

import asyncio
import signal
from pathlib import Path
import click

from xpressai.core.config import load_config
from xpressai.core.runtime import Runtime


def run_up(agents: list[str] | None = None, detach: bool = False) -> None:
    """Start the runtime and agents."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        click.echo("Run 'xpressai init' first.")
        return

    click.echo(click.style("Starting XpressAI...", fg="cyan", bold=True))

    # Load config
    config = load_config(config_path)

    # Filter agents if specified
    if agents:
        config.agents = [a for a in config.agents if a.name in agents]
        if not config.agents:
            click.echo(click.style(f"No matching agents found: {agents}", fg="yellow"))
            return

    # Show what we're starting
    click.echo(f"  Agents: {', '.join(a.name for a in config.agents)}")
    click.echo(f"  Isolation: {config.system.isolation}")

    if detach:
        click.echo()
        click.echo("Running in background mode...")
        _run_detached(config)
    else:
        asyncio.run(_run_foreground(config))


async def _run_foreground(config) -> None:
    """Run runtime in foreground with signal handling."""
    runtime = Runtime(config)

    # Handle Ctrl+C gracefully
    loop = asyncio.get_running_loop()
    stop_event = asyncio.Event()

    def signal_handler():
        click.echo()
        click.echo(click.style("Shutting down...", fg="yellow"))
        stop_event.set()

    loop.add_signal_handler(signal.SIGINT, signal_handler)
    loop.add_signal_handler(signal.SIGTERM, signal_handler)

    try:
        # Initialize and start
        await runtime.initialize()
        click.echo(click.style("Runtime initialized", fg="green"))

        await runtime.start()

        # Show status
        agents = await runtime.list_agents()
        for agent in agents:
            status_icon = "[running]" if agent.status == "running" else "[stopped]"
            color = "green" if agent.status == "running" else "red"
            click.echo(f"  {click.style(status_icon, fg=color)} {agent.name} ({agent.backend})")

        click.echo()
        click.echo(click.style("Runtime is running. Press Ctrl+C to stop.", fg="green"))
        click.echo()

        # Wait for stop signal
        await stop_event.wait()

    finally:
        await runtime.stop()
        click.echo(click.style("Stopped", fg="green"))


def _run_detached(config) -> None:
    """Run runtime as a background daemon."""
    import os
    import sys
    import logging

    # Fork to background
    pid = os.fork()
    if pid > 0:
        # Parent process
        click.echo(f"Started background process: PID {pid}")
        click.echo("Use 'xpressai status' to check status")
        click.echo("Use 'xpressai down' to stop")
        return

    # Child process - detach from terminal
    os.setsid()

    # Close standard file descriptors
    sys.stdin.close()

    # Setup log file
    log_path = Path.home() / ".xpressai" / "runtime.log"
    log_path.parent.mkdir(exist_ok=True)

    # Open log file for stdout/stderr
    log_file = open(log_path, "a")
    os.dup2(log_file.fileno(), sys.stdout.fileno())
    os.dup2(log_file.fileno(), sys.stderr.fileno())

    # Configure Python logging to write to the same file
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(levelname)s] [%(name)s] %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
        handlers=[
            logging.FileHandler(log_path),
        ],
        force=True,  # Override any existing config
    )

    # Write PID file
    pid_path = Path.home() / ".xpressai" / "runtime.pid"
    pid_path.write_text(str(os.getpid()))

    # Run
    asyncio.run(_run_background(config))


async def _run_background(config) -> None:
    """Background runtime loop with API server."""
    import threading

    runtime = Runtime(config)
    await runtime.initialize()
    await runtime.start()

    # Start API server in a thread for status queries
    def run_api():
        try:
            from xpressai.web.app import create_app
            import uvicorn

            app = create_app(runtime)
            uvicorn.run(app, host="127.0.0.1", port=8935, log_level="warning")
        except Exception as e:
            print(f"API server error: {e}")

    api_thread = threading.Thread(target=run_api, daemon=True)
    api_thread.start()

    # Keep running until killed
    while True:
        await asyncio.sleep(1)
