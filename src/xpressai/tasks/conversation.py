"""Task conversation management for XpressAI.

Manages message threads for tasks, enabling agent-user communication.
"""

from dataclasses import dataclass
from datetime import datetime
from typing import Literal
import logging

from xpressai.memory.database import Database
from xpressai.tasks.board import TaskBoard, TaskStatus

logger = logging.getLogger(__name__)

MessageRole = Literal["agent", "user", "system", "tool", "hook"]


@dataclass
class TaskMessage:
    """A message in a task conversation thread.

    Attributes:
        id: Message ID
        task_id: Associated task ID
        role: Who sent the message (agent/user/system)
        content: Message content
        timestamp: When the message was sent
    """

    id: int
    task_id: str
    role: MessageRole
    content: str
    timestamp: datetime


class ConversationManager:
    """Manages conversation threads for tasks.

    Provides methods to add messages, get conversation history,
    and handle the input request/response flow.
    """

    def __init__(self, db: Database, task_board: TaskBoard):
        """Initialize conversation manager.

        Args:
            db: Database instance
            task_board: TaskBoard instance for status updates
        """
        self.db = db
        self.task_board = task_board

    async def add_message(
        self,
        task_id: str,
        role: MessageRole,
        content: str,
    ) -> TaskMessage:
        """Add a message to a task's conversation.

        Args:
            task_id: Task ID
            role: Message role (agent/user/system)
            content: Message content

        Returns:
            Created message
        """
        with self.db.connect() as conn:
            cursor = conn.execute(
                """
                INSERT INTO task_messages (task_id, role, content)
                VALUES (?, ?, ?)
            """,
                (task_id, role, content),
            )
            message_id = cursor.lastrowid

            row = conn.execute(
                "SELECT * FROM task_messages WHERE id = ?", (message_id,)
            ).fetchone()

            return self._row_to_message(row)

    async def get_messages(self, task_id: str) -> list[TaskMessage]:
        """Get all messages for a task.

        Args:
            task_id: Task ID

        Returns:
            List of messages ordered by timestamp
        """
        with self.db.connect() as conn:
            rows = conn.execute(
                """
                SELECT * FROM task_messages
                WHERE task_id = ?
                ORDER BY timestamp ASC
            """,
                (task_id,),
            ).fetchall()

            return [self._row_to_message(row) for row in rows]

    async def request_input(self, task_id: str, agent_message: str) -> None:
        """Request input from user for a task.

        Sets task status to waiting_for_input and adds the agent's question.

        Args:
            task_id: Task ID
            agent_message: The question or prompt from the agent
        """
        # Add the agent's message
        await self.add_message(task_id, "agent", agent_message)

        # Update task status to waiting
        await self.task_board.update_status(task_id, TaskStatus.WAITING_FOR_INPUT)

        logger.info(f"Task {task_id} waiting for user input")

    async def provide_input(self, task_id: str, user_message: str) -> None:
        """Provide user input to a waiting task.

        Adds the user's message and sets task status back to pending.

        Args:
            task_id: Task ID
            user_message: The user's response
        """
        # Add the user's message
        await self.add_message(task_id, "user", user_message)

        # Update task status back to pending so agent can continue
        await self.task_board.update_status(task_id, TaskStatus.PENDING)

        logger.info(f"User input provided for task {task_id}")

    async def get_pending_input(self, task_id: str) -> str | None:
        """Check if there's a pending user response for a waiting task.

        Args:
            task_id: Task ID

        Returns:
            User's response if available, None if still waiting
        """
        messages = await self.get_messages(task_id)
        if not messages:
            return None

        # Get the last message
        last_message = messages[-1]

        # If the last message is from the user, return it
        if last_message.role == "user":
            return last_message.content

        return None

    async def get_conversation_context(self, task_id: str) -> str:
        """Get conversation history formatted for agent context.

        Only includes user and agent messages - excludes system/hook/tool
        messages which are internal and would cause exponential prompt growth.

        Args:
            task_id: Task ID

        Returns:
            Formatted conversation string for agent prompt
        """
        messages = await self.get_messages(task_id)
        if not messages:
            return ""

        # Only include user and agent messages to avoid exponential growth
        # System messages contain the full prompt which would recursively include
        # previous system messages, doubling in size each iteration
        relevant_roles = {"user", "agent"}
        relevant_messages = [m for m in messages if m.role in relevant_roles]

        if not relevant_messages:
            return ""

        lines = ["Previous conversation:"]
        for msg in relevant_messages:
            role_label = msg.role.upper()
            lines.append(f"[{role_label}]: {msg.content}")

        return "\n".join(lines)

    async def clear_messages(self, task_id: str) -> int:
        """Clear all messages for a task.

        Args:
            task_id: Task ID

        Returns:
            Number of messages deleted
        """
        with self.db.connect() as conn:
            cursor = conn.execute(
                "DELETE FROM task_messages WHERE task_id = ?",
                (task_id,),
            )
            count = cursor.rowcount
            logger.info(f"Cleared {count} messages for task {task_id}")
            return count

    def _row_to_message(self, row) -> TaskMessage:
        """Convert a database row to TaskMessage."""
        return TaskMessage(
            id=row["id"],
            task_id=row["task_id"],
            role=row["role"],
            content=row["content"],
            timestamp=datetime.fromisoformat(row["timestamp"])
            if row["timestamp"]
            else datetime.now(),
        )
