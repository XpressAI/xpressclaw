"""Task control tools for agent task management.

Provides tools for agents to signal task completion or failure.
"""

from __future__ import annotations

import logging
import threading
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.tools.registry import ToolRegistry

logger = logging.getLogger(__name__)

# Thread-safe shared state for task completion (works across SDK execution contexts)
_task_state_lock = threading.Lock()
_task_completion_state: dict[str, dict] = {}  # task_id -> {"completed": bool, "summary": str}

# Current task ID being executed (set by runner before execution)
_current_task_id: str | None = None


def set_current_task_id(task_id: str | None) -> None:
    """Set the current task ID being executed."""
    global _current_task_id
    _current_task_id = task_id


def get_current_task_id() -> str | None:
    """Get the current task ID being executed."""
    return _current_task_id


def reset_task_completion(task_id: str | None = None) -> None:
    """Reset task completion state for a new task."""
    tid = task_id or _current_task_id
    if tid:
        with _task_state_lock:
            _task_completion_state.pop(tid, None)


def is_task_completed(task_id: str | None = None) -> bool:
    """Check if the task was marked as completed."""
    tid = task_id or _current_task_id
    if not tid:
        return False
    with _task_state_lock:
        state = _task_completion_state.get(tid, {})
        return state.get("completed", False)


def get_completion_summary(task_id: str | None = None) -> str | None:
    """Get the completion summary if task was completed."""
    tid = task_id or _current_task_id
    if not tid:
        return None
    with _task_state_lock:
        state = _task_completion_state.get(tid, {})
        return state.get("summary")


def _mark_task_done(summary: str) -> None:
    """Internal: mark current task as done with summary."""
    tid = _current_task_id
    if tid:
        with _task_state_lock:
            _task_completion_state[tid] = {"completed": True, "summary": summary}
        logger.info(f"Task {tid} marked as done: {summary[:100]}...")
    else:
        logger.warning("complete_task/fail_task called but no current task ID set")


async def complete_task(summary: str) -> str:
    """Mark the current task as completed.

    This tool MUST be called when you have finished the task.
    Do not just respond with text - call this tool to signal completion.

    Args:
        summary: A brief summary of what was accomplished

    Returns:
        Confirmation message
    """
    _mark_task_done(summary)
    return f"Task completed: {summary}"


async def fail_task(reason: str) -> str:
    """Mark the current task as failed.

    Call this if you cannot complete the task for any reason.

    Args:
        reason: Explanation of why the task cannot be completed

    Returns:
        Confirmation message
    """
    _mark_task_done(f"FAILED: {reason}")
    return f"Task failed: {reason}"


async def register_task_control_tools(registry: ToolRegistry) -> None:
    """Register task control tools with the registry.

    Args:
        registry: The tool registry
    """
    from xpressai.tools.registry import ToolDefinition, ToolCategory

    registry.register_tool(
        ToolDefinition(
            name="complete_task",
            description=(
                "Mark the current task as COMPLETED. You MUST call this tool when you have "
                "finished the task. Do not just respond with text explaining what you did - "
                "actually call this tool to signal that the task is done."
            ),
            category=ToolCategory.CUSTOM,
            input_schema={
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "A brief summary of what was accomplished",
                    },
                },
                "required": ["summary"],
            },
            handler=complete_task,
            requires_confirmation=False,
            metadata={
                "changes_task_status": True,
            },
        )
    )

    registry.register_tool(
        ToolDefinition(
            name="fail_task",
            description=(
                "Mark the current task as FAILED. Call this if you cannot complete the task "
                "for any reason (missing information, permissions, etc.)."
            ),
            category=ToolCategory.CUSTOM,
            input_schema={
                "type": "object",
                "properties": {
                    "reason": {
                        "type": "string",
                        "description": "Explanation of why the task cannot be completed",
                    },
                },
                "required": ["reason"],
            },
            handler=fail_task,
            requires_confirmation=False,
            metadata={
                "changes_task_status": True,
            },
        )
    )
