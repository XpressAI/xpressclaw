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
    import threading

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

        # Start agents individually with status output
        click.echo("Starting agents...")
        agents = await runtime.list_agents()
        for agent in agents:
            click.echo(f"  Starting {click.style(agent.name, fg='cyan')} ({agent.backend})...", nl=False)
            try:
                await runtime.start_agent(agent.id)
                click.echo(click.style(" ready", fg="green"))
            except Exception as e:
                click.echo(click.style(f" failed: {e}", fg="red"))

        # Start API server in a thread for status queries
        api_thread = _start_api_server(runtime)

        # Show final status
        click.echo()
        agents = await runtime.list_agents()
        for agent in agents:
            status_icon = "[running]" if agent.status == "running" else "[stopped]"
            color = "green" if agent.status == "running" else "red"
            click.echo(f"  {click.style(status_icon, fg=color)} {agent.name} ({agent.backend})")

        click.echo()
        click.echo(click.style("Runtime is running. Press Ctrl+C to stop.", fg="green"))
        if api_thread:
            click.echo(click.style("  API available at http://127.0.0.1:8935", fg="cyan"))
        click.echo()

        # Wait for stop signal
        await stop_event.wait()

    finally:
        _stop_api_server()
        await runtime.stop()
        click.echo(click.style("Stopped", fg="green"))


def _get_port_pid(port: int) -> int | None:
    """Get the PID of the process LISTENING on a port, if any."""
    import subprocess
    try:
        # Try lsof first - filter for LISTEN state only to exclude client connections
        result = subprocess.run(
            ["lsof", "-ti", f":{port}", "-sTCP:LISTEN"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0 and result.stdout.strip():
            # May return multiple PIDs, get the first
            pid_str = result.stdout.strip().split("\n")[0]
            return int(pid_str)
    except (subprocess.TimeoutExpired, FileNotFoundError, ValueError):
        pass

    try:
        # Fallback to ss
        result = subprocess.run(
            ["ss", "-tlnp", f"sport = :{port}"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        if result.returncode == 0:
            # Parse output for pid=XXXX
            import re
            match = re.search(r'pid=(\d+)', result.stdout)
            if match:
                return int(match.group(1))
    except (subprocess.TimeoutExpired, FileNotFoundError, ValueError):
        pass

    return None


def _is_port_in_use(port: int, host: str = "127.0.0.1") -> bool:
    """Check if a port is already in use."""
    import socket
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        try:
            s.bind((host, port))
            return False
        except OSError:
            return True


def _wait_for_port(port: int, timeout: float = 10.0, interval: float = 0.5) -> bool:
    """Wait for a port to become available.

    Args:
        port: Port to wait for
        timeout: Maximum seconds to wait
        interval: Seconds between checks

    Returns:
        True if port became available, False if timeout
    """
    import time
    start = time.time()
    while time.time() - start < timeout:
        if not _is_port_in_use(port):
            return True
        time.sleep(interval)
    return False


# Global reference to uvicorn server and thread for clean shutdown
_uvicorn_server = None
_api_thread = None


def _start_api_server(runtime, port: int = 8935) -> "threading.Thread | None":
    """Start the API server in a background thread.

    Args:
        runtime: The runtime instance to serve
        port: Port to run on (default 8935)

    Returns:
        The thread running the server, or None if it couldn't start
    """
    global _uvicorn_server, _api_thread
    import threading
    import os

    # Check if port is already in use
    if _is_port_in_use(port):
        pid = _get_port_pid(port)
        current_pid = os.getpid()

        if pid and pid != current_pid:
            # Another process is using the port
            click.echo(click.style(f"  Port {port} in use by PID {pid}", fg="yellow"))
            click.echo(f"  (Run 'kill {pid}' or 'xpressai down' to stop it)")
            return None
        else:
            # Port in TIME_WAIT state (no active process), wait for it
            click.echo(f"  Waiting for port {port} to become available...")
            waited = 0
            while _is_port_in_use(port):
                import time
                time.sleep(1)
                waited += 1
                if waited % 5 == 0:
                    click.echo(f"  Still waiting for port {port}... ({waited}s)")
            click.echo(f"  Port {port} is now available")

    def run_api():
        global _uvicorn_server
        try:
            from xpressai.web.app import create_app
            import uvicorn

            app = create_app(runtime)
            config = uvicorn.Config(
                app,
                host="127.0.0.1",
                port=port,
                log_level="error",
            )
            _uvicorn_server = uvicorn.Server(config)
            _uvicorn_server.run()
        except ImportError:
            # FastAPI/uvicorn not installed, skip API server
            pass
        except Exception as e:
            # Don't print errors during shutdown
            if "Errno 98" not in str(e) and _uvicorn_server and not _uvicorn_server.should_exit:
                print(f"API server error: {e}")

    try:
        _api_thread = threading.Thread(target=run_api, daemon=True)
        _api_thread.start()
        return _api_thread
    except Exception:
        return None


def _stop_api_server(timeout: float = 5.0):
    """Stop the API server if running.

    Args:
        timeout: Seconds to wait for server to stop
    """
    global _uvicorn_server, _api_thread
    if _uvicorn_server:
        _uvicorn_server.should_exit = True

        # Wait for the thread to actually finish
        if _api_thread and _api_thread.is_alive():
            _api_thread.join(timeout=timeout)

        # Force close any remaining connections
        if _uvicorn_server and hasattr(_uvicorn_server, 'servers'):
            for server in _uvicorn_server.servers:
                if hasattr(server, 'close'):
                    server.close()

        _uvicorn_server = None
        _api_thread = None

        # Brief wait to let socket fully release
        import time
        time.sleep(0.2)


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
    runtime = Runtime(config)
    await runtime.initialize()
    await runtime.start()

    # Start API server in a thread for status queries
    _start_api_server(runtime)

    # Keep running until killed
    while True:
        await asyncio.sleep(1)
