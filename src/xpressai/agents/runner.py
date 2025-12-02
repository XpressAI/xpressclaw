"""Agent runner - executes tasks using agent backends.

The runner is the core loop that makes agents actually do work:
1. Polls for pending tasks assigned to the agent
2. Picks up tasks and executes them
3. Uses SOPs to guide execution when specified
4. Reports results back to the task board
"""

import asyncio
import logging
from datetime import datetime
from typing import Any

from xpressai.agents.base import AgentBackend
from xpressai.tasks.board import TaskBoard, Task, TaskStatus
from xpressai.tasks.sop import SOPManager, SOP

logger = logging.getLogger(__name__)


class AgentRunner:
    """Runs an agent, processing tasks from the task board.

    The runner continuously:
    1. Checks for pending tasks assigned to this agent
    2. Picks up the highest priority task
    3. Executes it (following SOP if specified)
    4. Marks it complete or failed
    """

    def __init__(
        self,
        agent_id: str,
        backend: AgentBackend,
        task_board: TaskBoard,
        sop_manager: SOPManager | None = None,
        poll_interval: float = 2.0,
    ):
        """Initialize the agent runner.

        Args:
            agent_id: ID of the agent
            backend: Agent backend for LLM interactions
            task_board: Task board to poll for work
            sop_manager: Optional SOP manager for workflow guidance
            poll_interval: How often to check for new tasks (seconds)
        """
        self.agent_id = agent_id
        self.backend = backend
        self.task_board = task_board
        self.sop_manager = sop_manager
        self.poll_interval = poll_interval

        self._running = False
        self._current_task: Task | None = None
        self._task: asyncio.Task | None = None

    async def start(self) -> None:
        """Start the agent runner loop."""
        if self._running:
            return

        self._running = True
        self._task = asyncio.create_task(self._run_loop())
        logger.info(f"Agent {self.agent_id} runner started")

    async def stop(self) -> None:
        """Stop the agent runner."""
        self._running = False

        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
            self._task = None

        logger.info(f"Agent {self.agent_id} runner stopped")

    async def _run_loop(self) -> None:
        """Main agent loop - poll for tasks and execute them."""
        logger.info(f"Agent {self.agent_id} entering run loop")

        while self._running:
            try:
                # Check for pending tasks
                task = await self._get_next_task()

                if task:
                    await self._execute_task(task)
                else:
                    # No tasks, wait before polling again
                    await asyncio.sleep(self.poll_interval)

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Agent {self.agent_id} loop error: {e}")
                await asyncio.sleep(self.poll_interval)

    async def _get_next_task(self) -> Task | None:
        """Get the next pending task for this agent."""
        tasks = await self.task_board.get_tasks(
            status=TaskStatus.PENDING,
            agent_id=self.agent_id,
            limit=1,
        )
        return tasks[0] if tasks else None

    async def _execute_task(self, task: Task) -> None:
        """Execute a single task."""
        logger.info(f"Agent {self.agent_id} starting task: {task.title}")
        self._current_task = task

        try:
            # Mark as in progress
            await self.task_board.update_status(
                task.id,
                TaskStatus.IN_PROGRESS,
                self.agent_id,
            )

            # Build the prompt
            prompt = await self._build_task_prompt(task)

            # Execute via backend
            response_parts = []
            async for chunk in self.backend.send(prompt):
                response_parts.append(chunk)

            response = "".join(response_parts)
            logger.info(f"Agent {self.agent_id} completed task: {task.title}")
            logger.debug(f"Response: {response[:200]}...")

            # Mark as completed
            await self.task_board.update_status(task.id, TaskStatus.COMPLETED)

        except Exception as e:
            logger.error(f"Agent {self.agent_id} task failed: {e}")

            # Mark as blocked (could retry later)
            await self.task_board.update_status(task.id, TaskStatus.BLOCKED)

        finally:
            self._current_task = None

    async def _build_task_prompt(self, task: Task) -> str:
        """Build the prompt for a task, incorporating SOP if specified."""
        parts = []

        # If task has an SOP, load and include it
        if task.sop_id and self.sop_manager:
            sop = self.sop_manager.get(task.sop_id)
            if sop:
                parts.append(self._format_sop_prompt(sop, task))
            else:
                logger.warning(f"SOP not found: {task.sop_id}")

        # Add the task itself
        parts.append(f"# Task: {task.title}")
        if task.description:
            parts.append(f"\n{task.description}")

        # Add any context
        if task.context:
            parts.append(f"\n## Context\n{task.context}")

        return "\n\n".join(parts)

    def _format_sop_prompt(self, sop: SOP, task: Task) -> str:
        """Format an SOP as part of the prompt."""
        parts = [f"# Standard Operating Procedure: {sop.name}"]

        if sop.summary:
            parts.append(f"\n{sop.summary}")

        if sop.steps:
            parts.append("\n## Steps to follow:")
            for i, step in enumerate(sop.steps, 1):
                parts.append(f"\n{i}. {step.prompt}")
                if step.tools:
                    parts.append(f"   Tools available: {', '.join(step.tools)}")

        return "\n".join(parts)

    @property
    def is_running(self) -> bool:
        """Whether the runner is active."""
        return self._running

    @property
    def current_task(self) -> Task | None:
        """The task currently being executed, if any."""
        return self._current_task
