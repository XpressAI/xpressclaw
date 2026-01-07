"""Activity logging for XpressAI.

Tracks events across the system for observability and debugging.
"""

from dataclasses import dataclass
from datetime import datetime
from enum import Enum
from typing import Any
import json
import logging

from xpressai.memory.database import Database

logger = logging.getLogger(__name__)


class EventType(str, Enum):
    """Types of activity events."""

    # Task events
    TASK_CREATED = "task.created"
    TASK_STARTED = "task.started"
    TASK_COMPLETED = "task.completed"
    TASK_FAILED = "task.failed"
    TASK_WAITING = "task.waiting"

    # Agent events
    AGENT_STARTED = "agent.started"
    AGENT_STOPPED = "agent.stopped"
    AGENT_ERROR = "agent.error"

    # Tool events
    TOOL_CALLED = "tool.called"
    TOOL_COMPLETED = "tool.completed"
    TOOL_FAILED = "tool.failed"

    # System events
    SYSTEM_STARTUP = "system.startup"
    SYSTEM_SHUTDOWN = "system.shutdown"

    # User events
    USER_MESSAGE = "user.message"


@dataclass
class ActivityEvent:
    """An activity event.

    Attributes:
        id: Event ID
        timestamp: When the event occurred
        event_type: Type of event
        agent_id: Related agent (if any)
        data: Additional event data
        session_id: Related session (if any)
    """

    id: int
    timestamp: datetime
    event_type: EventType
    agent_id: str | None
    data: dict[str, Any]
    session_id: str | None


class ActivityManager:
    """Manages activity logging and querying.

    Provides methods to log events and retrieve activity history.
    """

    def __init__(self, db: Database):
        """Initialize activity manager.

        Args:
            db: Database instance
        """
        self.db = db

    async def log(
        self,
        event_type: EventType,
        agent_id: str | None = None,
        data: dict[str, Any] | None = None,
        session_id: str | None = None,
    ) -> ActivityEvent:
        """Log an activity event.

        Args:
            event_type: Type of event
            agent_id: Related agent
            data: Additional event data
            session_id: Related session

        Returns:
            Created event
        """
        with self.db.connect() as conn:
            cursor = conn.execute(
                """
                INSERT INTO activity_logs (agent_id, event_type, event_data, session_id)
                VALUES (?, ?, ?, ?)
            """,
                (
                    agent_id,
                    event_type.value,
                    json.dumps(data) if data else None,
                    session_id,
                ),
            )
            event_id = cursor.lastrowid

            row = conn.execute(
                "SELECT * FROM activity_logs WHERE id = ?", (event_id,)
            ).fetchone()

            event = self._row_to_event(row)
            logger.debug(f"Logged activity: {event_type.value} - {data}")
            return event

    async def get_recent(self, limit: int = 50) -> list[ActivityEvent]:
        """Get recent activity events.

        Args:
            limit: Maximum number of events to return

        Returns:
            List of events, most recent first
        """
        with self.db.connect() as conn:
            rows = conn.execute(
                """
                SELECT * FROM activity_logs
                ORDER BY timestamp DESC
                LIMIT ?
            """,
                (limit,),
            ).fetchall()

            return [self._row_to_event(row) for row in rows]

    async def get_by_agent(
        self, agent_id: str, limit: int = 50
    ) -> list[ActivityEvent]:
        """Get activity events for a specific agent.

        Args:
            agent_id: Agent ID to filter by
            limit: Maximum number of events

        Returns:
            List of events for the agent
        """
        with self.db.connect() as conn:
            rows = conn.execute(
                """
                SELECT * FROM activity_logs
                WHERE agent_id = ?
                ORDER BY timestamp DESC
                LIMIT ?
            """,
                (agent_id, limit),
            ).fetchall()

            return [self._row_to_event(row) for row in rows]

    async def get_by_type(
        self, event_type: EventType, limit: int = 50
    ) -> list[ActivityEvent]:
        """Get activity events of a specific type.

        Args:
            event_type: Event type to filter by
            limit: Maximum number of events

        Returns:
            List of events of the given type
        """
        with self.db.connect() as conn:
            rows = conn.execute(
                """
                SELECT * FROM activity_logs
                WHERE event_type = ?
                ORDER BY timestamp DESC
                LIMIT ?
            """,
                (event_type.value, limit),
            ).fetchall()

            return [self._row_to_event(row) for row in rows]

    async def clear_old(self, days: int = 30) -> int:
        """Clear activity logs older than specified days.

        Args:
            days: Number of days to keep

        Returns:
            Number of events deleted
        """
        with self.db.connect() as conn:
            cursor = conn.execute(
                """
                DELETE FROM activity_logs
                WHERE timestamp < datetime('now', ?)
            """,
                (f"-{days} days",),
            )
            count = cursor.rowcount
            logger.info(f"Cleared {count} old activity logs")
            return count

    def _row_to_event(self, row) -> ActivityEvent:
        """Convert a database row to ActivityEvent."""
        data = {}
        if row["event_data"]:
            try:
                data = json.loads(row["event_data"])
            except json.JSONDecodeError:
                pass

        return ActivityEvent(
            id=row["id"],
            timestamp=datetime.fromisoformat(row["timestamp"])
            if row["timestamp"]
            else datetime.now(),
            event_type=EventType(row["event_type"]),
            agent_id=row["agent_id"],
            data=data,
            session_id=row["session_id"],
        )
