"""Task board management for XpressAI.

Implements a Kanban-style task board for agents.
"""

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any
import uuid
import json

from xpressai.memory.database import Database
from xpressai.core.exceptions import TaskNotFoundError


class TaskStatus(str, Enum):
    """Status of a task."""

    PENDING = "pending"
    IN_PROGRESS = "in_progress"
    BLOCKED = "blocked"
    COMPLETED = "completed"
    CANCELLED = "cancelled"


@dataclass
class Task:
    """A task on the board.

    Attributes:
        id: Unique task ID
        title: Task title
        description: Optional description
        status: Current status
        priority: Priority (higher = more important)
        agent_id: Assigned agent
        parent_task_id: Parent task for subtasks
        sop_id: Associated SOP if any
        created_at: Creation timestamp
        updated_at: Last update timestamp
        completed_at: Completion timestamp
        context: Additional context data
    """

    id: str
    title: str
    description: str | None = None
    status: TaskStatus = TaskStatus.PENDING
    priority: int = 0
    agent_id: str | None = None
    parent_task_id: str | None = None
    sop_id: str | None = None
    created_at: datetime = field(default_factory=datetime.now)
    updated_at: datetime = field(default_factory=datetime.now)
    completed_at: datetime | None = None
    context: dict[str, Any] = field(default_factory=dict)


class TaskBoard:
    """Manages tasks for all agents.

    Provides CRUD operations and status management for tasks.
    """

    def __init__(self, db: Database):
        """Initialize task board.

        Args:
            db: Database instance
        """
        self.db = db

    async def create_task(
        self,
        title: str,
        description: str | None = None,
        agent_id: str | None = None,
        parent_task_id: str | None = None,
        sop_id: str | None = None,
        priority: int = 0,
        context: dict[str, Any] | None = None,
    ) -> Task:
        """Create a new task.

        Args:
            title: Task title
            description: Optional description
            agent_id: Assigned agent
            parent_task_id: Parent task ID
            sop_id: Associated SOP
            priority: Task priority
            context: Additional context

        Returns:
            Created task
        """
        task_id = str(uuid.uuid4())
        now = datetime.now()

        task = Task(
            id=task_id,
            title=title,
            description=description,
            agent_id=agent_id,
            parent_task_id=parent_task_id,
            sop_id=sop_id,
            priority=priority,
            created_at=now,
            updated_at=now,
            context=context or {},
        )

        with self.db.connect() as conn:
            conn.execute(
                """
                INSERT INTO tasks 
                (id, title, description, status, priority, agent_id, parent_task_id, sop_id, context)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
                (
                    task_id,
                    title,
                    description,
                    task.status.value,
                    priority,
                    agent_id,
                    parent_task_id,
                    sop_id,
                    json.dumps(context) if context else None,
                ),
            )

        return task

    async def get_task(self, task_id: str) -> Task:
        """Get a task by ID.

        Args:
            task_id: Task ID

        Returns:
            Task instance

        Raises:
            TaskNotFoundError: If task doesn't exist
        """
        with self.db.connect() as conn:
            row = conn.execute("SELECT * FROM tasks WHERE id = ?", (task_id,)).fetchone()

            if row is None:
                raise TaskNotFoundError(f"Task not found: {task_id}", {"task_id": task_id})

            return self._row_to_task(row)

    async def update_status(
        self,
        task_id: str,
        status: TaskStatus,
        agent_id: str | None = None,
    ) -> Task:
        """Update a task's status.

        Args:
            task_id: Task ID
            status: New status
            agent_id: Agent making the change

        Returns:
            Updated task
        """
        with self.db.connect() as conn:
            now = datetime.now()

            # Get current task
            row = conn.execute("SELECT * FROM tasks WHERE id = ?", (task_id,)).fetchone()

            if row is None:
                raise TaskNotFoundError(f"Task not found: {task_id}", {"task_id": task_id})

            # Build update
            updates = ["status = ?", "updated_at = ?"]
            params: list[Any] = [status.value, now.isoformat()]

            if status == TaskStatus.IN_PROGRESS and agent_id:
                updates.append("agent_id = ?")
                params.append(agent_id)

            if status == TaskStatus.COMPLETED:
                updates.append("completed_at = ?")
                params.append(now.isoformat())

            params.append(task_id)

            conn.execute(f"UPDATE tasks SET {', '.join(updates)} WHERE id = ?", params)

            # Return updated task
            return await self.get_task(task_id)

    async def delete_task(self, task_id: str) -> None:
        """Delete a task.

        Args:
            task_id: Task ID
        """
        with self.db.connect() as conn:
            conn.execute("DELETE FROM tasks WHERE id = ?", (task_id,))

    async def get_tasks(
        self,
        status: TaskStatus | None = None,
        agent_id: str | None = None,
        parent_task_id: str | None = None,
        limit: int = 100,
    ) -> list[Task]:
        """Query tasks with filters.

        Args:
            status: Filter by status
            agent_id: Filter by agent
            parent_task_id: Filter by parent
            limit: Maximum results

        Returns:
            List of matching tasks
        """
        with self.db.connect() as conn:
            sql = "SELECT * FROM tasks WHERE 1=1"
            params: list[Any] = []

            if status:
                sql += " AND status = ?"
                params.append(status.value)

            if agent_id:
                sql += " AND agent_id = ?"
                params.append(agent_id)

            if parent_task_id:
                sql += " AND parent_task_id = ?"
                params.append(parent_task_id)

            sql += " ORDER BY priority DESC, created_at ASC LIMIT ?"
            params.append(limit)

            rows = conn.execute(sql, params).fetchall()

            return [self._row_to_task(row) for row in rows]

    async def get_counts(self) -> dict[str, int]:
        """Get task counts by status.

        Returns:
            Dict of status -> count
        """
        with self.db.connect() as conn:
            rows = conn.execute("""
                SELECT status, COUNT(*) as count
                FROM tasks
                GROUP BY status
            """).fetchall()

            counts = {
                "pending": 0,
                "in_progress": 0,
                "completed": 0,
                "blocked": 0,
                "cancelled": 0,
            }

            for row in rows:
                counts[row["status"]] = row["count"]

            return counts

    async def get_subtasks(self, task_id: str) -> list[Task]:
        """Get subtasks of a task.

        Args:
            task_id: Parent task ID

        Returns:
            List of subtasks
        """
        return await self.get_tasks(parent_task_id=task_id)

    async def assign_task(self, task_id: str, agent_id: str) -> Task:
        """Assign a task to an agent.

        Args:
            task_id: Task ID
            agent_id: Agent to assign

        Returns:
            Updated task
        """
        with self.db.connect() as conn:
            conn.execute(
                """
                UPDATE tasks
                SET agent_id = ?, updated_at = ?
                WHERE id = ?
            """,
                (agent_id, datetime.now().isoformat(), task_id),
            )

        return await self.get_task(task_id)

    def _row_to_task(self, row) -> Task:
        """Convert a database row to Task."""
        context = {}
        if row["context"]:
            try:
                context = json.loads(row["context"])
            except json.JSONDecodeError:
                pass

        return Task(
            id=row["id"],
            title=row["title"],
            description=row["description"],
            status=TaskStatus(row["status"]),
            priority=row["priority"],
            agent_id=row["agent_id"],
            parent_task_id=row["parent_task_id"],
            sop_id=row["sop_id"],
            created_at=datetime.fromisoformat(row["created_at"])
            if row["created_at"]
            else datetime.now(),
            updated_at=datetime.fromisoformat(row["updated_at"])
            if row["updated_at"]
            else datetime.now(),
            completed_at=datetime.fromisoformat(row["completed_at"])
            if row["completed_at"]
            else None,
            context=context,
        )
