"""Task board management for XpressAI."""

from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import Any
import uuid

from xpressai.memory.database import Database


class TaskStatus(str, Enum):
    PENDING = "pending"
    IN_PROGRESS = "in_progress"
    BLOCKED = "blocked"
    COMPLETED = "completed"
    CANCELLED = "cancelled"


@dataclass
class Task:
    """A task on the board."""
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
    """Manages tasks for all agents."""
    
    def __init__(self, db: Database):
        self.db = db
    
    async def create_task(
        self,
        title: str,
        description: str | None = None,
        agent_id: str | None = None,
        parent_task_id: str | None = None,
        sop_id: str | None = None,
        priority: int = 0,
    ) -> Task:
        """Create a new task."""
        task_id = str(uuid.uuid4())
        
        task = Task(
            id=task_id,
            title=title,
            description=description,
            agent_id=agent_id,
            parent_task_id=parent_task_id,
            sop_id=sop_id,
            priority=priority,
        )
        
        with self.db.connect() as conn:
            conn.execute("""
                INSERT INTO tasks (id, title, description, status, priority, agent_id, parent_task_id, sop_id)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """, (
                task_id, title, description, task.status.value,
                priority, agent_id, parent_task_id, sop_id
            ))
        
        return task
    
    async def update_status(
        self,
        task_id: str,
        status: TaskStatus,
        agent_id: str | None = None,
    ) -> Task | None:
        """Update a task's status."""
        with self.db.connect() as conn:
            now = datetime.now()
            
            # Get current task
            row = conn.execute(
                "SELECT * FROM tasks WHERE id = ?", (task_id,)
            ).fetchone()
            
            if row is None:
                return None
            
            # Update
            updates = ["status = ?", "updated_at = ?"]
            params: list[Any] = [status.value, now]
            
            if status == TaskStatus.IN_PROGRESS and agent_id:
                updates.append("agent_id = ?")
                params.append(agent_id)
            
            if status == TaskStatus.COMPLETED:
                updates.append("completed_at = ?")
                params.append(now)
            
            params.append(task_id)
            
            conn.execute(
                f"UPDATE tasks SET {', '.join(updates)} WHERE id = ?",
                params
            )
            
            return Task(
                id=task_id,
                title=row["title"],
                description=row["description"],
                status=status,
                priority=row["priority"],
                agent_id=agent_id or row["agent_id"],
            )
    
    async def get_tasks(
        self,
        status: TaskStatus | None = None,
        agent_id: str | None = None,
        limit: int = 100,
    ) -> list[Task]:
        """Query tasks with filters."""
        with self.db.connect() as conn:
            sql = "SELECT * FROM tasks WHERE 1=1"
            params: list[Any] = []
            
            if status:
                sql += " AND status = ?"
                params.append(status.value)
            
            if agent_id:
                sql += " AND agent_id = ?"
                params.append(agent_id)
            
            sql += " ORDER BY priority DESC, created_at ASC LIMIT ?"
            params.append(limit)
            
            rows = conn.execute(sql, params).fetchall()
            
            return [
                Task(
                    id=row["id"],
                    title=row["title"],
                    description=row["description"],
                    status=TaskStatus(row["status"]),
                    priority=row["priority"],
                    agent_id=row["agent_id"],
                )
                for row in rows
            ]
    
    async def get_counts(self) -> dict[str, int]:
        """Get task counts by status."""
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
