"""Budget management and enforcement for XpressAI.

Tracks spending, enforces limits, and handles budget exceeded scenarios.
"""

from dataclasses import dataclass
from datetime import datetime, timedelta
from decimal import Decimal
from typing import Any
import logging

from xpressai.memory.database import Database
from xpressai.core.config import BudgetConfig
from xpressai.core.exceptions import BudgetExceededError, BudgetError
from xpressai.budget.tracker import UsageRecord, CostCalculator

logger = logging.getLogger(__name__)


@dataclass
class BudgetState:
    """Current budget state for an agent.

    Attributes:
        agent_id: Agent ID
        daily_spent: Amount spent today
        daily_reset_at: When daily budget resets
        monthly_spent: Amount spent this month
        monthly_reset_at: When monthly budget resets
        total_spent: Total amount ever spent
        is_paused: Whether the agent is paused due to budget
        pause_reason: Reason for pause
    """

    agent_id: str
    daily_spent: Decimal = Decimal("0")
    daily_reset_at: datetime | None = None
    monthly_spent: Decimal = Decimal("0")
    monthly_reset_at: datetime | None = None
    total_spent: Decimal = Decimal("0")
    is_paused: bool = False
    pause_reason: str | None = None


class BudgetManager:
    """Manages budgets and enforces limits.

    Tracks usage, checks limits before operations, and handles
    budget exceeded scenarios based on configured policy.
    """

    def __init__(self, db: Database, config: BudgetConfig):
        """Initialize budget manager.

        Args:
            db: Database instance
            config: Budget configuration
        """
        self.db = db
        self.config = config
        self.cost_calculator = CostCalculator()

    async def record_usage(
        self,
        agent_id: str,
        model: str,
        input_tokens: int,
        output_tokens: int,
        operation: str = "query",
        session_id: str | None = None,
    ) -> UsageRecord:
        """Record usage and update budget state.

        Args:
            agent_id: ID of the agent
            model: Model used
            input_tokens: Number of input tokens
            output_tokens: Number of output tokens
            operation: Type of operation
            session_id: Optional session ID

        Returns:
            Usage record

        Raises:
            BudgetExceededError: If budget is exceeded after recording
        """
        cost = self.cost_calculator.calculate(model, input_tokens, output_tokens)

        with self.db.connect() as conn:
            # Insert usage record
            conn.execute(
                """
                INSERT INTO usage_logs 
                (agent_id, model, input_tokens, output_tokens, cost_usd, operation, session_id)
                VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
                (agent_id, model, input_tokens, output_tokens, float(cost), operation, session_id),
            )

            record_id = conn.execute("SELECT last_insert_rowid()").fetchone()[0]

        # Update budget state
        await self._update_state(agent_id, cost)

        return UsageRecord(
            id=record_id,
            agent_id=agent_id,
            timestamp=datetime.now(),
            model=model,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            cost_usd=cost,
            operation=operation,
            session_id=session_id,
        )

    async def check_budget(self, agent_id: str, estimated_cost: Decimal = Decimal("0")) -> bool:
        """Check if an operation is within budget.

        Args:
            agent_id: ID of the agent
            estimated_cost: Estimated cost of the operation

        Returns:
            True if within budget

        Raises:
            BudgetExceededError: If budget would be exceeded
        """
        state = await self._get_state(agent_id)

        # Check if paused
        if state.is_paused:
            raise BudgetExceededError(
                f"Agent {agent_id} is paused: {state.pause_reason}",
                agent_id=agent_id,
                limit_type="paused",
                limit=0,
                current=float(state.daily_spent),
            )

        # Check daily limit
        if self.config.daily:
            projected = state.daily_spent + estimated_cost
            if projected >= self.config.daily:
                if self.config.on_exceeded == "stop":
                    raise BudgetExceededError(
                        f"Daily budget would be exceeded: ${projected} >= ${self.config.daily}",
                        agent_id=agent_id,
                        limit_type="daily",
                        limit=float(self.config.daily),
                        current=float(state.daily_spent),
                    )
                return False

        # Check monthly limit
        if self.config.monthly:
            projected = state.monthly_spent + estimated_cost
            if projected >= self.config.monthly:
                if self.config.on_exceeded == "stop":
                    raise BudgetExceededError(
                        f"Monthly budget would be exceeded: ${projected} >= ${self.config.monthly}",
                        agent_id=agent_id,
                        limit_type="monthly",
                        limit=float(self.config.monthly),
                        current=float(state.monthly_spent),
                    )
                return False

        return True

    async def get_state(self, agent_id: str) -> BudgetState:
        """Get budget state for an agent.

        Args:
            agent_id: ID of the agent

        Returns:
            Budget state
        """
        return await self._get_state(agent_id)

    async def _get_state(self, agent_id: str) -> BudgetState:
        """Get budget state, resetting if needed."""
        now = datetime.now()

        with self.db.connect() as conn:
            row = conn.execute(
                "SELECT * FROM budget_state WHERE agent_id = ?", (agent_id,)
            ).fetchone()

            if row is None:
                # Initialize state
                state = BudgetState(agent_id=agent_id)
                await self._save_state(state)
                return state

            state = BudgetState(
                agent_id=agent_id,
                daily_spent=Decimal(str(row["daily_spent"])),
                daily_reset_at=datetime.fromisoformat(row["daily_reset_at"])
                if row["daily_reset_at"]
                else None,
                monthly_spent=Decimal(str(row["monthly_spent"])),
                monthly_reset_at=datetime.fromisoformat(row["monthly_reset_at"])
                if row["monthly_reset_at"]
                else None,
                total_spent=Decimal(str(row["total_spent"])),
                is_paused=bool(row["is_paused"]),
                pause_reason=row["pause_reason"],
            )

            # Check if daily reset is needed
            if state.daily_reset_at and now >= state.daily_reset_at:
                state.daily_spent = Decimal("0")
                state.daily_reset_at = now.replace(hour=0, minute=0, second=0) + timedelta(days=1)

                # Auto-resume if paused for daily limit
                if state.is_paused and "Daily" in (state.pause_reason or ""):
                    state.is_paused = False
                    state.pause_reason = None

                await self._save_state(state)

            # Check if monthly reset is needed
            if state.monthly_reset_at and now >= state.monthly_reset_at:
                state.monthly_spent = Decimal("0")
                # Next month, first day
                if now.month == 12:
                    next_reset = now.replace(
                        year=now.year + 1, month=1, day=1, hour=0, minute=0, second=0
                    )
                else:
                    next_reset = now.replace(month=now.month + 1, day=1, hour=0, minute=0, second=0)
                state.monthly_reset_at = next_reset

                # Auto-resume if paused for monthly limit
                if state.is_paused and "Monthly" in (state.pause_reason or ""):
                    state.is_paused = False
                    state.pause_reason = None

                await self._save_state(state)

            return state

    async def _update_state(self, agent_id: str, cost: Decimal) -> None:
        """Update budget state after usage."""
        state = await self._get_state(agent_id)
        now = datetime.now()

        # Initialize reset times if needed
        if state.daily_reset_at is None:
            state.daily_reset_at = now.replace(hour=0, minute=0, second=0) + timedelta(days=1)

        if state.monthly_reset_at is None:
            if now.month == 12:
                state.monthly_reset_at = now.replace(
                    year=now.year + 1, month=1, day=1, hour=0, minute=0, second=0
                )
            else:
                state.monthly_reset_at = now.replace(
                    month=now.month + 1, day=1, hour=0, minute=0, second=0
                )

        # Update totals
        state.daily_spent += cost
        state.monthly_spent += cost
        state.total_spent += cost

        # Save state
        await self._save_state(state)

        # Check limits and handle exceeded
        await self._check_limits(agent_id, state)

    async def _save_state(self, state: BudgetState) -> None:
        """Save budget state to database."""
        with self.db.connect() as conn:
            conn.execute(
                """
                INSERT OR REPLACE INTO budget_state 
                (agent_id, daily_spent, daily_reset_at, monthly_spent, monthly_reset_at, 
                 total_spent, is_paused, pause_reason)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """,
                (
                    state.agent_id,
                    float(state.daily_spent),
                    state.daily_reset_at.isoformat() if state.daily_reset_at else None,
                    float(state.monthly_spent),
                    state.monthly_reset_at.isoformat() if state.monthly_reset_at else None,
                    float(state.total_spent),
                    int(state.is_paused),
                    state.pause_reason,
                ),
            )

    async def _check_limits(self, agent_id: str, state: BudgetState) -> None:
        """Check if limits are exceeded and handle accordingly."""
        exceeded = False
        reason = None

        # Check daily limit
        if self.config.daily and state.daily_spent >= self.config.daily:
            exceeded = True
            reason = f"Daily limit ${self.config.daily} exceeded"

        # Check monthly limit
        if self.config.monthly and state.monthly_spent >= self.config.monthly:
            exceeded = True
            reason = f"Monthly limit ${self.config.monthly} exceeded"

        if exceeded:
            await self._handle_exceeded(agent_id, reason)

    async def _handle_exceeded(self, agent_id: str, reason: str | None) -> None:
        """Handle budget exceeded based on policy."""
        action = self.config.on_exceeded

        if action == "pause":
            with self.db.connect() as conn:
                conn.execute(
                    """
                    UPDATE budget_state 
                    SET is_paused = 1, pause_reason = ?
                    WHERE agent_id = ?
                """,
                    (reason, agent_id),
                )
            logger.warning(f"Agent {agent_id} paused: {reason}")

        elif action == "alert":
            logger.warning(f"Budget alert for {agent_id}: {reason}")

        elif action == "degrade":
            logger.info(f"Agent {agent_id} degraded to local model: {reason}")
            # The backend should check this and switch models

        elif action == "stop":
            raise BudgetExceededError(
                reason or "Budget exceeded",
                agent_id=agent_id,
                limit_type="exceeded",
                limit=0,
                current=0,
            )

    async def resume(self, agent_id: str) -> None:
        """Resume a paused agent.

        Args:
            agent_id: ID of the agent
        """
        with self.db.connect() as conn:
            conn.execute(
                """
                UPDATE budget_state
                SET is_paused = 0, pause_reason = NULL
                WHERE agent_id = ?
            """,
                (agent_id,),
            )
        logger.info(f"Agent {agent_id} resumed")

    async def set_limit(
        self,
        agent_id: str | None = None,
        daily: Decimal | None = None,
        monthly: Decimal | None = None,
    ) -> None:
        """Set budget limits.

        Args:
            agent_id: Optional agent ID (None = global)
            daily: Daily limit
            monthly: Monthly limit
        """
        if daily is not None:
            self.config.daily = daily
        if monthly is not None:
            self.config.monthly = monthly

    async def get_summary(self, agent_id: str | None = None) -> dict[str, Any]:
        """Get budget summary.

        Args:
            agent_id: Optional agent ID for specific agent

        Returns:
            Budget summary dict
        """
        with self.db.connect() as conn:
            if agent_id:
                state = await self._get_state(agent_id)

                return {
                    "agent_id": agent_id,
                    "daily_spent": float(state.daily_spent),
                    "daily_limit": float(self.config.daily) if self.config.daily else None,
                    "monthly_spent": float(state.monthly_spent),
                    "monthly_limit": float(self.config.monthly) if self.config.monthly else None,
                    "total_spent": float(state.total_spent),
                    "is_paused": state.is_paused,
                    "pause_reason": state.pause_reason,
                }

            # Aggregate all agents
            row = conn.execute("""
                SELECT 
                    SUM(daily_spent) as daily,
                    SUM(monthly_spent) as monthly,
                    SUM(total_spent) as total
                FROM budget_state
            """).fetchone()

            return {
                "daily_spent": row["daily"] or 0,
                "daily_limit": float(self.config.daily) if self.config.daily else None,
                "monthly_spent": row["monthly"] or 0,
                "monthly_limit": float(self.config.monthly) if self.config.monthly else None,
                "total_spent": row["total"] or 0,
            }

    async def get_usage_history(
        self,
        agent_id: str | None = None,
        limit: int = 100,
        since: datetime | None = None,
    ) -> list[UsageRecord]:
        """Get usage history.

        Args:
            agent_id: Optional agent filter
            limit: Maximum records
            since: Optional start time

        Returns:
            List of usage records
        """
        with self.db.connect() as conn:
            sql = "SELECT * FROM usage_logs WHERE 1=1"
            params: list[Any] = []

            if agent_id:
                sql += " AND agent_id = ?"
                params.append(agent_id)

            if since:
                sql += " AND timestamp >= ?"
                params.append(since.isoformat())

            sql += " ORDER BY timestamp DESC LIMIT ?"
            params.append(limit)

            rows = conn.execute(sql, params).fetchall()

            return [
                UsageRecord(
                    id=row["id"],
                    agent_id=row["agent_id"],
                    timestamp=datetime.fromisoformat(row["timestamp"]),
                    model=row["model"],
                    input_tokens=row["input_tokens"],
                    output_tokens=row["output_tokens"],
                    cost_usd=Decimal(str(row["cost_usd"])),
                    operation=row["operation"],
                    session_id=row["session_id"],
                )
                for row in rows
            ]
