"""Ask user tool for agent-user communication.

Provides a mechanism for agents to ask questions and wait for user responses.
"""

from __future__ import annotations

import asyncio
import logging
from contextvars import ContextVar
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.tools.registry import ToolRegistry
    from xpressai.tasks.conversation import ConversationManager

logger = logging.getLogger(__name__)

# Context variable to track current task during execution
current_task_id: ContextVar[str | None] = ContextVar("current_task_id", default=None)

# Global reference to conversation manager (set during initialization)
_conversation_manager: ConversationManager | None = None


def set_conversation_manager(manager: ConversationManager) -> None:
    """Set the global conversation manager reference.

    Args:
        manager: ConversationManager instance
    """
    global _conversation_manager
    _conversation_manager = manager


def get_conversation_manager() -> ConversationManager | None:
    """Get the global conversation manager reference."""
    return _conversation_manager


async def ask_user(question: str, timeout: float = 3600.0) -> str:
    """Ask the user a question and wait for their response.

    This tool allows agents to request input from users during task execution.
    The task will be marked as 'waiting_for_input' until the user responds.

    Args:
        question: The question to ask the user
        timeout: Maximum time to wait for response in seconds (default 1 hour)

    Returns:
        The user's response

    Raises:
        RuntimeError: If no task context or conversation manager available
        TimeoutError: If user doesn't respond within timeout
    """
    task_id = current_task_id.get()
    if not task_id:
        raise RuntimeError(
            "ask_user can only be called within a task context. "
            "No current task_id found."
        )

    manager = get_conversation_manager()
    if not manager:
        raise RuntimeError(
            "ConversationManager not initialized. "
            "Make sure the runtime is properly started."
        )

    logger.info(f"Task {task_id} requesting user input: {question[:50]}...")

    # Request input from user (sets status to waiting_for_input)
    await manager.request_input(task_id, question)

    # Poll for user response
    poll_interval = 1.0  # Check every second
    elapsed = 0.0

    while elapsed < timeout:
        response = await manager.get_pending_input(task_id)
        if response:
            logger.info(f"Task {task_id} received user input")
            return response

        await asyncio.sleep(poll_interval)
        elapsed += poll_interval

    raise TimeoutError(f"User did not respond within {timeout} seconds")


async def register_ask_user_tool(registry: ToolRegistry) -> None:
    """Register the ask_user tool with the registry.

    Args:
        registry: The tool registry
    """
    from xpressai.tools.registry import ToolDefinition, ToolCategory

    registry.register_tool(
        ToolDefinition(
            name="ask_user",
            description=(
                "Ask the user a question and wait for their response. "
                "Use this when you need clarification, approval, or additional "
                "information from the user to continue with a task."
            ),
            category=ToolCategory.CUSTOM,
            input_schema={
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user",
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Maximum seconds to wait for response (default: 3600)",
                        "default": 3600.0,
                    },
                },
                "required": ["question"],
            },
            handler=ask_user,
            requires_confirmation=False,
            metadata={
                "blocks_execution": True,
                "changes_task_status": True,
            },
        )
    )
