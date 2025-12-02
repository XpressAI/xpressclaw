"""Shell command execution tools for XpressAI agents.

Provides controlled shell command execution with:
- Allowlist-based command filtering
- Timeout controls
- Output capture
"""

from __future__ import annotations

import asyncio
import shlex
import logging
from dataclasses import dataclass
from typing import Any, Dict, List, Optional, TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.tools.registry import ToolRegistry

logger = logging.getLogger(__name__)


# Default allowed commands (safe subset)
DEFAULT_ALLOWED_COMMANDS = [
    # Version control
    "git",
    # Package managers
    "npm",
    "yarn",
    "pnpm",
    "pip",
    "uv",
    "cargo",
    "go",
    # Build tools
    "make",
    "cmake",
    "gradle",
    "mvn",
    # Language runtimes
    "python",
    "python3",
    "node",
    "ruby",
    "go",
    "cargo",
    "rustc",
    # Common utilities
    "ls",
    "cat",
    "head",
    "tail",
    "grep",
    "find",
    "wc",
    "sort",
    "uniq",
    "echo",
    "printf",
    "date",
    "pwd",
    "whoami",
    "mkdir",
    "touch",
    "cp",
    "mv",
    # Network (read-only)
    "curl",
    "wget",
    # Testing
    "pytest",
    "jest",
    "mocha",
    "rspec",
    # Linting
    "eslint",
    "ruff",
    "black",
    "prettier",
]


@dataclass
class ShellResult:
    """Result of a shell command execution."""

    command: str
    exit_code: int
    stdout: str
    stderr: str
    timed_out: bool = False


async def execute_command(
    command: str,
    timeout: int = 60,
    working_directory: Optional[str] = None,
    allowed_commands: Optional[List[str]] = None,
) -> ShellResult:
    """Execute a shell command.

    Args:
        command: The command to execute
        timeout: Timeout in seconds (default 60)
        working_directory: Directory to run command in
        allowed_commands: List of allowed command prefixes

    Returns:
        ShellResult with output and exit code

    Raises:
        ValueError: If command is not allowed
    """
    # Parse the command
    try:
        parts = shlex.split(command)
    except ValueError as e:
        raise ValueError(f"Invalid command syntax: {e}")

    if not parts:
        raise ValueError("Empty command")

    # Check if command is allowed
    allowed = allowed_commands or DEFAULT_ALLOWED_COMMANDS
    cmd_name = parts[0]

    if not _is_command_allowed(cmd_name, allowed):
        raise ValueError(
            f"Command '{cmd_name}' is not in the allowed list. "
            f"Allowed commands: {', '.join(sorted(allowed)[:10])}..."
        )

    logger.info(f"Executing command: {command}")

    # Execute the command
    try:
        process = await asyncio.create_subprocess_shell(
            command,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=working_directory,
        )

        try:
            stdout, stderr = await asyncio.wait_for(process.communicate(), timeout=timeout)

            return ShellResult(
                command=command,
                exit_code=process.returncode or 0,
                stdout=stdout.decode("utf-8", errors="replace"),
                stderr=stderr.decode("utf-8", errors="replace"),
            )

        except asyncio.TimeoutError:
            process.kill()
            await process.wait()

            return ShellResult(
                command=command,
                exit_code=-1,
                stdout="",
                stderr=f"Command timed out after {timeout} seconds",
                timed_out=True,
            )

    except Exception as e:
        logger.error(f"Command execution failed: {e}")
        return ShellResult(
            command=command,
            exit_code=-1,
            stdout="",
            stderr=str(e),
        )


def _is_command_allowed(cmd_name: str, allowed: List[str]) -> bool:
    """Check if a command is in the allowed list.

    Args:
        cmd_name: The command name
        allowed: List of allowed commands

    Returns:
        True if allowed
    """
    # Get just the command name (strip path)
    cmd_base = cmd_name.split("/")[-1]

    # Check against allowed list
    for allowed_cmd in allowed:
        if cmd_base == allowed_cmd or cmd_name == allowed_cmd:
            return True

    return False


async def register_shell_tools(registry: ToolRegistry) -> None:
    """Register shell tools with the registry.

    Args:
        registry: The tool registry
    """
    from xpressai.tools.registry import ToolDefinition, ToolCategory

    async def execute_wrapper(
        command: str,
        timeout: int = 60,
        working_directory: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Wrapper that returns dict for MCP."""
        result = await execute_command(
            command=command,
            timeout=timeout,
            working_directory=working_directory,
        )
        return {
            "command": result.command,
            "exit_code": result.exit_code,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "timed_out": result.timed_out,
        }

    registry.register_tool(
        ToolDefinition(
            name="execute_command",
            description=(
                "Execute a shell command. Only certain commands are allowed for safety. "
                "Allowed commands include: git, npm, pip, python, make, ls, cat, grep, etc."
            ),
            category=ToolCategory.SHELL,
            input_schema={
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The shell command to execute"},
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 60)",
                        "default": 60,
                    },
                    "working_directory": {
                        "type": "string",
                        "description": "Directory to run the command in",
                    },
                },
                "required": ["command"],
            },
            handler=execute_wrapper,
            requires_confirmation=True,
        )
    )
