"""Budget tracking and enforcement for XpressAI."""

from dataclasses import dataclass
from datetime import datetime, timedelta
from decimal import Decimal
from typing import Any

from xpressai.memory.database import Database
from xpressai.core.config import BudgetConfig


@dataclass
class UsageRecord:
    """A record of API usage."""
    id: int
    agent_id: str
    timestamp: datetime
    model: str
    input_tokens: int
    output_tokens: int
    cost_usd: Decimal
    operation: str


@dataclass
class BudgetState:
    """Current budget state for an agent."""
    agent_id: str
    daily_spent: Decimal = Decimal("0")
    daily_reset_at: datetime | None = None
    total_spent: Decimal = Decimal("0")
    is_paused: bool = False
    pause_reason: str | None = None


class CostCalculator:
    """Calculates costs for different models."""
    
    # Prices per 1M tokens
    PRICING = {
        "claude-sonnet": {"input": 3.00, "output": 15.00},
        "claude-haiku": {"input": 0.25, "output": 1.25},
        "gpt-4o": {"input": 2.50, "output": 10.00},
        "gpt-4o-mini": {"input": 0.15, "output": 0.60},
        "local": {"input": 0.00, "output": 0.00},
    }
    
    def calculate(
        self,
        model: str,
        input_tokens: int,
        output_tokens: int,
    ) -> Decimal:
        """Calculate cost for token usage."""
        pricing = self.PRICING.get(model, {"input": 0, "output": 0})
        
        input_cost = Decimal(str(pricing["input"])) * input_tokens / 1_000_000
        output_cost = Decimal(str(pricing["output"])) * output_tokens / 1_000_000
        
        return input_cost + output_cost


class BudgetManager:
    """Tracks and enforces budgets."""
    
    def __init__(self, db: Database, config: BudgetConfig):
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
    ) -> UsageRecord:
        """Record usage and update budget state."""
        cost = self.cost_calculator.calculate(model, input_tokens, output_tokens)
        
        with self.db.connect() as conn:
            # Insert usage record
            conn.execute("""
                INSERT INTO usage_logs (agent_id, model, input_tokens, output_tokens, cost_usd, operation)
                VALUES (?, ?, ?, ?, ?, ?)
            """, (agent_id, model, input_tokens, output_tokens, float(cost), operation))
            
            # Update budget state
            await self._update_state(agent_id, cost)
        
        return UsageRecord(
            id=0,
            agent_id=agent_id,
            timestamp=datetime.now(),
            model=model,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            cost_usd=cost,
            operation=operation,
        )
    
    async def _update_state(self, agent_id: str, cost: Decimal) -> None:
        """Update budget state after usage."""
        state = await self._get_state(agent_id)
        now = datetime.now()
        
        # Reset daily if needed
        if state.daily_reset_at is None or now >= state.daily_reset_at:
            state.daily_spent = Decimal("0")
            tomorrow = now.replace(hour=0, minute=0, second=0) + timedelta(days=1)
            state.daily_reset_at = tomorrow
        
        # Update totals
        state.daily_spent += cost
        state.total_spent += cost
        
        # Save state
        with self.db.connect() as conn:
            conn.execute("""
                INSERT OR REPLACE INTO budget_state 
                (agent_id, daily_spent, daily_reset_at, total_spent, is_paused, pause_reason)
                VALUES (?, ?, ?, ?, ?, ?)
            """, (
                agent_id,
                float(state.daily_spent),
                state.daily_reset_at,
                float(state.total_spent),
                state.is_paused,
                state.pause_reason,
            ))
        
        # Check limits
        await self._check_limits(agent_id, state)
    
    async def _get_state(self, agent_id: str) -> BudgetState:
        """Get budget state for an agent."""
        with self.db.connect() as conn:
            row = conn.execute(
                "SELECT * FROM budget_state WHERE agent_id = ?",
                (agent_id,)
            ).fetchone()
            
            if row:
                return BudgetState(
                    agent_id=agent_id,
                    daily_spent=Decimal(str(row["daily_spent"])),
                    daily_reset_at=row["daily_reset_at"],
                    total_spent=Decimal(str(row["total_spent"])),
                    is_paused=bool(row["is_paused"]),
                    pause_reason=row["pause_reason"],
                )
            
            return BudgetState(agent_id=agent_id)
    
    async def _check_limits(self, agent_id: str, state: BudgetState) -> None:
        """Check if budget limits are exceeded."""
        exceeded = False
        reason = None
        
        if self.config.daily and state.daily_spent >= self.config.daily:
            exceeded = True
            reason = f"Daily limit ${self.config.daily} exceeded"
        
        if self.config.monthly and state.total_spent >= self.config.monthly:
            exceeded = True
            reason = f"Monthly limit ${self.config.monthly} exceeded"
        
        if exceeded:
            await self._handle_exceeded(agent_id, reason)
    
    async def _handle_exceeded(self, agent_id: str, reason: str | None) -> None:
        """Handle budget exceeded based on policy."""
        action = self.config.on_exceeded
        
        if action == "pause":
            with self.db.connect() as conn:
                conn.execute("""
                    UPDATE budget_state 
                    SET is_paused = 1, pause_reason = ?
                    WHERE agent_id = ?
                """, (reason, agent_id))
    
    async def get_summary(self, agent_id: str | None = None) -> dict[str, Any]:
        """Get budget summary."""
        with self.db.connect() as conn:
            if agent_id:
                row = conn.execute(
                    "SELECT * FROM budget_state WHERE agent_id = ?",
                    (agent_id,)
                ).fetchone()
                
                if row:
                    return {
                        "agent_id": agent_id,
                        "daily_spent": row["daily_spent"],
                        "total_spent": row["total_spent"],
                        "is_paused": bool(row["is_paused"]),
                    }
                return {"agent_id": agent_id, "daily_spent": 0, "total_spent": 0}
            
            # Aggregate all agents
            row = conn.execute("""
                SELECT SUM(daily_spent) as daily, SUM(total_spent) as total
                FROM budget_state
            """).fetchone()
            
            return {
                "total_spent": row["total"] or 0,
                "limit": float(self.config.daily) if self.config.daily else None,
            }
