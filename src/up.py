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
        click.echo(click.style("⚠️  No xpressai.yaml found.", fg="yellow"))
        click.echo("Run 'xpressai init' first.")
        return
    
    click.echo(click.style("⚡ Starting XpressAI...", fg="cyan", bold=True))
    
    # Load config
    config = load_config(config_path)
    
    # Filter agents if specified
    if agents:
        config.agents = [a for a in config.agents if a.name in agents]
        if not config.agents:
            click.echo(click.style(f"⚠️  No matching agents found: {agents}", fg="yellow"))
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
        click.echo(click.style("✓ Runtime initialized", fg="green"))
        
        await runtime.start()
        
        # Show status
        agents = await runtime.list_agents()
        for agent in agents:
            status_icon = "🟢" if agent.status == "running" else "🔴"
            click.echo(f"  {status_icon} {agent.name} ({agent.backend})")
        
        click.echo()
        click.echo(click.style("Runtime is running. Press Ctrl+C to stop.", fg="green"))
        click.echo()
        
        # Wait for stop signal
        await stop_event.wait()
        
    finally:
        await runtime.stop()
        click.echo(click.style("✓ Stopped", fg="green"))


def _run_detached(config) -> None:
    """Run runtime as a background daemon."""
    import os
    import sys
    
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
    sys.stdout.close()
    sys.stderr.close()
    
    # Redirect to log file
    log_path = Path.home() / ".xpressai" / "runtime.log"
    sys.stdout = open(log_path, "a")
    sys.stderr = sys.stdout
    
    # Write PID file
    pid_path = Path.home() / ".xpressai" / "runtime.pid"
    pid_path.write_text(str(os.getpid()))
    
    # Run
    asyncio.run(_run_background(config))


async def _run_background(config) -> None:
    """Background runtime loop."""
    runtime = Runtime(config)
    await runtime.initialize()
    await runtime.start()
    
    # Keep running until killed
    while True:
        await asyncio.sleep(1)
