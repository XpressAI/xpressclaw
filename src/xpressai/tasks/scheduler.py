"""Task scheduler for XpressAI.

Supports cron-style scheduled tasks using APScheduler.
Schedules are persisted to the database.
"""

import logging
from datetime import datetime
from typing import Callable, Any
from dataclasses import dataclass, field

from apscheduler.schedulers.asyncio import AsyncIOScheduler
from apscheduler.triggers.cron import CronTrigger
from apscheduler.job import Job

from xpressai.tasks.board import TaskBoard, Task
from xpressai.memory.database import Database

logger = logging.getLogger(__name__)


@dataclass
class ScheduledTask:
    """A scheduled task definition.

    Attributes:
        id: Unique schedule ID
        name: Human-readable name
        cron: Cron expression (e.g., "0 9 * * *" for 9am daily)
        agent_id: Agent to assign the task to
        title: Task title (can include {date}, {time} placeholders)
        description: Optional task description
        enabled: Whether the schedule is active
        last_run: Last time a task was created from this schedule
        run_count: Number of times this schedule has triggered
    """

    id: str
    name: str
    cron: str
    agent_id: str
    title: str
    description: str | None = None
    enabled: bool = True
    last_run: datetime | None = None
    run_count: int = 0
    created_at: datetime = field(default_factory=datetime.now)


class TaskScheduler:
    """Manages scheduled task creation.

    Uses APScheduler to trigger task creation at specified times.
    Schedules are persisted to the database.
    """

    def __init__(self, task_board: TaskBoard, db: Database | None = None):
        """Initialize the scheduler.

        Args:
            task_board: Task board to create tasks on
            db: Database for persistence (optional)
        """
        self.task_board = task_board
        self.db = db
        self._scheduler = AsyncIOScheduler()
        self._schedules: dict[str, ScheduledTask] = {}
        self._started = False

    def start(self) -> None:
        """Start the scheduler and load persisted schedules."""
        if not self._started:
            self._scheduler.start()
            self._started = True
            self._load_schedules()
            logger.info("Task scheduler started")

    def stop(self) -> None:
        """Stop the scheduler."""
        if self._started:
            self._scheduler.shutdown(wait=False)
            self._started = False
            logger.info("Task scheduler stopped")

    def _load_schedules(self) -> None:
        """Load schedules from database."""
        if not self.db:
            return

        with self.db.connect() as conn:
            rows = conn.execute("SELECT * FROM schedules WHERE enabled = 1").fetchall()

            for row in rows:
                schedule = ScheduledTask(
                    id=row["id"],
                    name=row["name"],
                    cron=row["cron"],
                    agent_id=row["agent_id"],
                    title=row["title"],
                    description=row["description"],
                    enabled=bool(row["enabled"]),
                    last_run=datetime.fromisoformat(row["last_run"]) if row["last_run"] else None,
                    run_count=row["run_count"],
                    created_at=datetime.fromisoformat(row["created_at"])
                    if row["created_at"]
                    else datetime.now(),
                )
                self._schedules[schedule.id] = schedule

                # Register with APScheduler
                try:
                    self._scheduler.add_job(
                        self._trigger_task,
                        CronTrigger.from_crontab(schedule.cron),
                        id=schedule.id,
                        kwargs={"schedule_id": schedule.id},
                        replace_existing=True,
                    )
                except Exception as e:
                    logger.warning(f"Failed to load schedule {schedule.id}: {e}")

        logger.info(f"Loaded {len(self._schedules)} schedules from database")

    def _save_schedule(self, schedule: ScheduledTask) -> None:
        """Save a schedule to the database."""
        if not self.db:
            return

        with self.db.connect() as conn:
            conn.execute(
                """
                INSERT OR REPLACE INTO schedules 
                (id, name, cron, agent_id, title, description, enabled, last_run, run_count, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    schedule.id,
                    schedule.name,
                    schedule.cron,
                    schedule.agent_id,
                    schedule.title,
                    schedule.description,
                    1 if schedule.enabled else 0,
                    schedule.last_run.isoformat() if schedule.last_run else None,
                    schedule.run_count,
                    schedule.created_at.isoformat(),
                ),
            )

    def _delete_schedule(self, schedule_id: str) -> None:
        """Delete a schedule from the database."""
        if not self.db:
            return

        with self.db.connect() as conn:
            conn.execute("DELETE FROM schedules WHERE id = ?", (schedule_id,))

    async def add_schedule(
        self,
        schedule_id: str,
        name: str,
        cron: str,
        agent_id: str,
        title: str,
        description: str | None = None,
    ) -> ScheduledTask:
        """Add a new scheduled task.

        Args:
            schedule_id: Unique ID for this schedule
            name: Human-readable name
            cron: Cron expression
            agent_id: Agent to assign tasks to
            title: Task title template
            description: Optional description

        Returns:
            The created ScheduledTask
        """
        schedule = ScheduledTask(
            id=schedule_id,
            name=name,
            cron=cron,
            agent_id=agent_id,
            title=title,
            description=description,
        )

        self._schedules[schedule_id] = schedule
        self._save_schedule(schedule)

        # Add job to APScheduler
        self._scheduler.add_job(
            self._trigger_task,
            CronTrigger.from_crontab(cron),
            id=schedule_id,
            kwargs={"schedule_id": schedule_id},
            replace_existing=True,
        )

        logger.info(f"Added schedule '{name}' ({cron}) for agent {agent_id}")
        return schedule

    async def remove_schedule(self, schedule_id: str) -> bool:
        """Remove a scheduled task.

        Args:
            schedule_id: Schedule ID to remove

        Returns:
            True if removed, False if not found
        """
        if schedule_id not in self._schedules:
            return False

        try:
            self._scheduler.remove_job(schedule_id)
        except Exception:
            pass

        del self._schedules[schedule_id]
        self._delete_schedule(schedule_id)
        logger.info(f"Removed schedule {schedule_id}")
        return True

    async def enable_schedule(self, schedule_id: str) -> bool:
        """Enable a schedule.

        Args:
            schedule_id: Schedule ID

        Returns:
            True if enabled, False if not found
        """
        if schedule_id not in self._schedules:
            return False

        schedule = self._schedules[schedule_id]
        schedule.enabled = True

        try:
            self._scheduler.resume_job(schedule_id)
        except Exception:
            # Re-add if job was removed
            self._scheduler.add_job(
                self._trigger_task,
                CronTrigger.from_crontab(schedule.cron),
                id=schedule_id,
                kwargs={"schedule_id": schedule_id},
                replace_existing=True,
            )

        return True

    async def disable_schedule(self, schedule_id: str) -> bool:
        """Disable a schedule.

        Args:
            schedule_id: Schedule ID

        Returns:
            True if disabled, False if not found
        """
        if schedule_id not in self._schedules:
            return False

        self._schedules[schedule_id].enabled = False

        try:
            self._scheduler.pause_job(schedule_id)
        except Exception:
            pass

        return True

    def list_schedules(self) -> list[ScheduledTask]:
        """List all schedules.

        Returns:
            List of scheduled tasks
        """
        return list(self._schedules.values())

    def get_schedule(self, schedule_id: str) -> ScheduledTask | None:
        """Get a schedule by ID.

        Args:
            schedule_id: Schedule ID

        Returns:
            ScheduledTask or None
        """
        return self._schedules.get(schedule_id)

    def get_next_run(self, schedule_id: str) -> datetime | None:
        """Get the next run time for a schedule.

        Args:
            schedule_id: Schedule ID

        Returns:
            Next run datetime or None
        """
        try:
            job = self._scheduler.get_job(schedule_id)
            if job and job.next_run_time:
                return job.next_run_time
        except Exception:
            pass
        return None

    async def _trigger_task(self, schedule_id: str) -> None:
        """Triggered by APScheduler to create a task.

        Args:
            schedule_id: Schedule that triggered
        """
        schedule = self._schedules.get(schedule_id)
        if not schedule:
            logger.warning(f"Schedule {schedule_id} not found")
            return

        if not schedule.enabled:
            logger.debug(f"Schedule {schedule_id} is disabled, skipping")
            return

        # Format title with date/time placeholders
        now = datetime.now()
        title = schedule.title.format(
            date=now.strftime("%Y-%m-%d"),
            time=now.strftime("%H:%M"),
            datetime=now.strftime("%Y-%m-%d %H:%M"),
        )

        description = schedule.description
        if description:
            description = description.format(
                date=now.strftime("%Y-%m-%d"),
                time=now.strftime("%H:%M"),
                datetime=now.strftime("%Y-%m-%d %H:%M"),
            )

        # Create the task
        task = await self.task_board.create_task(
            title=title,
            description=description,
            agent_id=schedule.agent_id,
            context={"scheduled": True, "schedule_id": schedule_id},
        )

        # Update schedule stats
        schedule.last_run = now
        schedule.run_count += 1
        self._save_schedule(schedule)

        logger.info(
            f"Scheduled task created: '{title}' for agent {schedule.agent_id} "
            f"(schedule: {schedule.name})"
        )

    async def trigger_now(self, schedule_id: str) -> Task | None:
        """Manually trigger a scheduled task immediately.

        Args:
            schedule_id: Schedule ID to trigger

        Returns:
            Created task or None if schedule not found
        """
        schedule = self._schedules.get(schedule_id)
        if not schedule:
            return None

        # Temporarily enable if disabled
        was_enabled = schedule.enabled
        schedule.enabled = True

        await self._trigger_task(schedule_id)

        # Restore enabled state
        schedule.enabled = was_enabled

        # Return the most recent task for this agent
        tasks = await self.task_board.get_tasks(
            agent_id=schedule.agent_id,
            limit=1,
        )
        return tasks[0] if tasks else None
