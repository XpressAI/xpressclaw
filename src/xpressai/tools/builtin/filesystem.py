"""Filesystem tools for XpressAI agents.

Provides file operations like read, write, list, search.
All operations are sandboxed to configured workspace directories.
"""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from typing import Any, Dict, List, TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.tools.registry import ToolRegistry, ToolCategory, ToolDefinition


# Workspace directory (configurable via set_workspace)
_workspace: Path | None = None


def set_workspace(path: Path) -> None:
    """Set the workspace directory for file operations."""
    global _workspace
    _workspace = path


def get_workspace() -> Path:
    """Get the current workspace directory."""
    if _workspace is not None:
        return _workspace
    # Default to current directory
    return Path.cwd()


async def read_file(path: str, encoding: str = "utf-8") -> str:
    """Read contents of a file.

    Args:
        path: Path to the file to read
        encoding: File encoding (default utf-8)

    Returns:
        File contents as string
    """
    file_path = _resolve_path(path)

    if not file_path.exists():
        raise FileNotFoundError(f"File not found: {path}")

    if not file_path.is_file():
        raise ValueError(f"Not a file: {path}")

    return await asyncio.to_thread(file_path.read_text, encoding=encoding)


async def write_file(path: str, content: str, encoding: str = "utf-8") -> str:
    """Write contents to a file.

    Args:
        path: Path to the file to write
        content: Content to write
        encoding: File encoding (default utf-8)

    Returns:
        Success message
    """
    file_path = _resolve_path(path)

    # Create parent directories if needed
    file_path.parent.mkdir(parents=True, exist_ok=True)

    await asyncio.to_thread(file_path.write_text, content, encoding=encoding)
    return f"Successfully wrote {len(content)} bytes to {file_path}"


async def list_directory(path: str = ".", recursive: bool = False) -> Dict[str, Any]:
    """List contents of a directory.

    Args:
        path: Directory path to list
        recursive: Whether to list recursively

    Returns:
        Dict with directory path and list of file/directory entries
    """
    dir_path = _resolve_path(path)

    if not dir_path.exists():
        raise FileNotFoundError(f"Directory not found: {path}")

    if not dir_path.is_dir():
        raise ValueError(f"Not a directory: {path}")

    entries = []

    def collect_entries(base: Path, relative_to: Path):
        for item in base.iterdir():
            entry = {
                "name": item.name,
                "path": str(item),  # Absolute path
                "type": "directory" if item.is_dir() else "file",
            }
            if item.is_file():
                entry["size"] = item.stat().st_size
            entries.append(entry)

            if recursive and item.is_dir():
                collect_entries(item, relative_to)

    await asyncio.to_thread(collect_entries, dir_path, dir_path)
    return {
        "directory": str(dir_path),
        "entries": entries,
    }


async def search_files(pattern: str, path: str = ".", max_results: int = 100) -> Dict[str, Any]:
    """Search for files matching a pattern.

    Args:
        pattern: Glob pattern to match
        path: Directory to search in
        max_results: Maximum number of results

    Returns:
        Dict with search directory and list of matching absolute paths
    """
    dir_path = _resolve_path(path)

    if not dir_path.exists():
        raise FileNotFoundError(f"Directory not found: {path}")

    def do_search():
        results = []
        for match in dir_path.glob(pattern):
            if len(results) >= max_results:
                break
            results.append(str(match))  # Absolute path
        return results

    matches = await asyncio.to_thread(do_search)
    return {
        "search_directory": str(dir_path),
        "pattern": pattern,
        "matches": matches,
    }


async def delete_file(path: str) -> str:
    """Delete a file.

    Args:
        path: Path to the file to delete

    Returns:
        Success message
    """
    file_path = _resolve_path(path)

    if not file_path.exists():
        raise FileNotFoundError(f"File not found: {file_path}")

    if file_path.is_dir():
        raise ValueError(f"Cannot delete directory with this tool: {file_path}")

    await asyncio.to_thread(file_path.unlink)
    return f"Successfully deleted {file_path}"


async def create_directory(path: str) -> str:
    """Create a directory.

    Args:
        path: Path for the new directory

    Returns:
        Success message
    """
    dir_path = _resolve_path(path)

    if dir_path.exists():
        raise ValueError(f"Path already exists: {dir_path}")

    await asyncio.to_thread(dir_path.mkdir, parents=True, exist_ok=True)
    return f"Successfully created directory {dir_path}"


def _resolve_path(path: str) -> Path:
    """Resolve and validate a path within the workspace.

    Args:
        path: User-provided path

    Returns:
        Resolved absolute path

    Raises:
        ValueError: If path escapes the workspace
    """
    # Use configured workspace
    workspace = get_workspace()
    workspace.mkdir(parents=True, exist_ok=True)

    # Expand ~ to home directory first
    expanded_path = os.path.expanduser(path)

    # Resolve the path
    if Path(expanded_path).is_absolute():
        resolved = Path(expanded_path).resolve()
    else:
        resolved = (workspace / expanded_path).resolve()

    # Check that it's within workspace
    try:
        resolved.relative_to(workspace)
    except ValueError:
        raise ValueError(f"Path escapes workspace: {path}")

    return resolved


async def register_filesystem_tools(registry: ToolRegistry) -> None:
    """Register filesystem tools with the registry.

    Args:
        registry: The tool registry
    """
    from xpressai.tools.registry import ToolDefinition, ToolCategory

    registry.register_tool(
        ToolDefinition(
            name="read_file",
            description="Read the contents of a file. Call this tool directly - do not use execute_command for file operations.",
            category=ToolCategory.FILESYSTEM,
            input_schema={
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file (use simple names like 'file.txt', not full paths)"},
                    "encoding": {
                        "type": "string",
                        "description": "File encoding (default: utf-8)",
                        "default": "utf-8",
                    },
                },
                "required": ["path"],
            },
            handler=read_file,
        )
    )

    registry.register_tool(
        ToolDefinition(
            name="write_file",
            description="Write/create a file with the specified content. Call this tool directly - do not use execute_command for file operations.",
            category=ToolCategory.FILESYSTEM,
            input_schema={
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file (use simple names like 'file.txt', not full paths)"},
                    "content": {"type": "string", "description": "Content to write to the file"},
                    "encoding": {
                        "type": "string",
                        "description": "File encoding (default: utf-8)",
                        "default": "utf-8",
                    },
                },
                "required": ["path", "content"],
            },
            handler=write_file,
            requires_confirmation=True,
        )
    )

    registry.register_tool(
        ToolDefinition(
            name="list_directory",
            description="List contents of a directory. Call this tool directly - do not use execute_command.",
            category=ToolCategory.FILESYSTEM,
            input_schema={
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list",
                        "default": ".",
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "Whether to list recursively",
                        "default": False,
                    },
                },
            },
            handler=list_directory,
        )
    )

    registry.register_tool(
        ToolDefinition(
            name="search_files",
            description="Search for files matching a glob pattern",
            category=ToolCategory.FILESYSTEM,
            input_schema={
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match (e.g., '**/*.py')",
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in",
                        "default": ".",
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results",
                        "default": 100,
                    },
                },
                "required": ["pattern"],
            },
            handler=search_files,
        )
    )

    registry.register_tool(
        ToolDefinition(
            name="delete_file",
            description="Delete a file",
            category=ToolCategory.FILESYSTEM,
            input_schema={
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file to delete"}
                },
                "required": ["path"],
            },
            handler=delete_file,
            requires_confirmation=True,
        )
    )

    registry.register_tool(
        ToolDefinition(
            name="create_directory",
            description="Create a new directory",
            category=ToolCategory.FILESYSTEM,
            input_schema={
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path for the new directory"}
                },
                "required": ["path"],
            },
            handler=create_directory,
        )
    )
