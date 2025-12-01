# ADR-010: Budget and Rate Limiting

## Status
Accepted

## Context

Agents spending money is a real concern:
- LLM API calls cost money (tokens)
- Runaway agents can rack up significant bills
- Users need visibility into spending
- Different agents may have different budgets

We need:
- Usage tracking per agent
- Budget limits with enforcement
- Rate limiting to prevent spikes
- Configurable policies when limits are hit

## Decision

We will implement a **budget and rate limiting system** with configurable policies.

### Cost Tracking

```python
from dataclasses import dataclass
from datetime import datetime, timedelta
from decimal import Decimal
from enum import Enum

@dataclass
class UsageRecord:
    id: int
    agent_id: str
    timestamp: datetime
    
    # Model info
    model: str
    provider: str  # "anthropic", "openai", "local"
    
    # Token usage
    input_tokens: int
    output_tokens: int
    
    # Cost
    cost_usd: Decimal
    
    # Context
    operation: str  # "query", "tool_call", "memory_embed"
    session_id: str | None = None

class CostCalculator:
    """Calculates costs for different models."""
    
    # Prices per 1M tokens (as of 2024)
    PRICING = {
        "claude-sonnet-4": {"input": 3.00, "output": 15.00},
        "claude-haiku": {"input": 0.25, "output": 1.25},
        "gpt-4o": {"input": 2.50, "output": 10.00},
        "gpt-4o-mini": {"input": 0.15, "output": 0.60},
        "local": {"input": 0.00, "output": 0.00},  # Free!
    }
    
    def calculate(
        self, 
        model: str, 
        input_tokens: int, 
        output_tokens: int
    ) -> Decimal:
        pricing = self.PRICING.get(model, {"input": 0, "output": 0})
        
        input_cost = Decimal(pricing["input"]) * input_tokens / 1_000_000
        output_cost = Decimal(pricing["output"]) * output_tokens / 1_000_000
        
        return input_cost + output_cost
```

### Budget Enforcement

```python
class BudgetExceededAction(str, Enum):
    PAUSE = "pause"       # Pause agent, wait for user
    ALERT = "alert"       # Continue but notify user
    DEGRADE = "degrade"   # Switch to cheaper model
    STOP = "stop"         # Stop agent entirely

@dataclass
class BudgetConfig:
    daily_limit: Decimal | None = None
    monthly_limit: Decimal | None = None
    per_task_limit: Decimal | None = None
    
    on_exceeded: BudgetExceededAction = BudgetExceededAction.PAUSE
    
    # For "degrade" action
    fallback_model: str | None = "local"
    
    # Warnings
    warn_at_percent: int = 80  # Warn at 80% usage

@dataclass
class BudgetState:
    agent_id: str
    
    daily_spent: Decimal = Decimal("0")
    daily_reset_at: datetime | None = None
    
    monthly_spent: Decimal = Decimal("0")
    monthly_reset_at: datetime | None = None
    
    current_task_spent: Decimal = Decimal("0")
    current_task_id: str | None = None
    
    is_paused: bool = False
    pause_reason: str | None = None
    
    degraded_model: str | None = None

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
        session_id: str | None = None,
    ) -> UsageRecord:
        """Record usage and check budget."""
        
        cost = self.cost_calculator.calculate(model, input_tokens, output_tokens)
        
        record = UsageRecord(
            id=0,  # Auto-assigned
            agent_id=agent_id,
            timestamp=datetime.now(),
            model=model,
            provider=self._get_provider(model),
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            cost_usd=cost,
            operation=operation,
            session_id=session_id,
        )
        
        await self._save_record(record)
        await self._update_budget_state(agent_id, cost)
        
        # Check limits
        await self._check_limits(agent_id)
        
        return record
    
    async def _update_budget_state(self, agent_id: str, cost: Decimal) -> None:
        state = await self._get_state(agent_id)
        
        # Reset daily if needed
        now = datetime.now()
        if state.daily_reset_at is None or now >= state.daily_reset_at:
            state.daily_spent = Decimal("0")
            state.daily_reset_at = now.replace(
                hour=0, minute=0, second=0, microsecond=0
            ) + timedelta(days=1)
        
        # Reset monthly if needed
        if state.monthly_reset_at is None or now >= state.monthly_reset_at:
            state.monthly_spent = Decimal("0")
            next_month = (now.replace(day=1) + timedelta(days=32)).replace(day=1)
            state.monthly_reset_at = next_month
        
        # Update totals
        state.daily_spent += cost
        state.monthly_spent += cost
        state.current_task_spent += cost
        
        await self._save_state(state)
    
    async def _check_limits(self, agent_id: str) -> None:
        state = await self._get_state(agent_id)
        
        exceeded = False
        reason = None
        
        if self.config.daily_limit and state.daily_spent >= self.config.daily_limit:
            exceeded = True
            reason = f"Daily limit ${self.config.daily_limit} exceeded"
        
        if self.config.monthly_limit and state.monthly_spent >= self.config.monthly_limit:
            exceeded = True
            reason = f"Monthly limit ${self.config.monthly_limit} exceeded"
        
        if self.config.per_task_limit and state.current_task_spent >= self.config.per_task_limit:
            exceeded = True
            reason = f"Task limit ${self.config.per_task_limit} exceeded"
        
        if exceeded:
            await self._handle_exceeded(agent_id, reason)
        
        # Check warnings
        await self._check_warnings(agent_id, state)
    
    async def _handle_exceeded(self, agent_id: str, reason: str) -> None:
        action = self.config.on_exceeded
        
        if action == BudgetExceededAction.PAUSE:
            state = await self._get_state(agent_id)
            state.is_paused = True
            state.pause_reason = reason
            await self._save_state(state)
            await self._emit_event("budget.paused", agent_id, reason)
        
        elif action == BudgetExceededAction.ALERT:
            await self._emit_event("budget.alert", agent_id, reason)
        
        elif action == BudgetExceededAction.DEGRADE:
            state = await self._get_state(agent_id)
            state.degraded_model = self.config.fallback_model
            await self._save_state(state)
            await self._emit_event("budget.degraded", agent_id, reason)
        
        elif action == BudgetExceededAction.STOP:
            await self._emit_event("budget.stopped", agent_id, reason)
            raise BudgetExceededError(reason)
    
    async def get_summary(self, agent_id: str) -> dict:
        """Get budget summary for an agent."""
        state = await self._get_state(agent_id)
        
        return {
            "daily": {
                "spent": float(state.daily_spent),
                "limit": float(self.config.daily_limit) if self.config.daily_limit else None,
                "percent": self._calc_percent(state.daily_spent, self.config.daily_limit),
                "resets_at": state.daily_reset_at.isoformat() if state.daily_reset_at else None,
            },
            "monthly": {
                "spent": float(state.monthly_spent),
                "limit": float(self.config.monthly_limit) if self.config.monthly_limit else None,
                "percent": self._calc_percent(state.monthly_spent, self.config.monthly_limit),
                "resets_at": state.monthly_reset_at.isoformat() if state.monthly_reset_at else None,
            },
            "status": "paused" if state.is_paused else "active",
            "pause_reason": state.pause_reason,
            "degraded_model": state.degraded_model,
        }
    
    async def resume(self, agent_id: str) -> None:
        """Resume a paused agent."""
        state = await self._get_state(agent_id)
        state.is_paused = False
        state.pause_reason = None
        await self._save_state(state)
        await self._emit_event("budget.resumed", agent_id)
```

### Rate Limiting

```python
from collections import defaultdict
import asyncio

@dataclass
class RateLimitConfig:
    requests_per_minute: int = 60
    tokens_per_minute: int = 100_000
    concurrent_requests: int = 5

class RateLimiter:
    """Token bucket rate limiter."""
    
    def __init__(self, config: RateLimitConfig):
        self.config = config
        self._request_tokens: dict[str, float] = defaultdict(lambda: config.requests_per_minute)
        self._token_tokens: dict[str, float] = defaultdict(lambda: config.tokens_per_minute)
        self._concurrent: dict[str, int] = defaultdict(int)
        self._semaphores: dict[str, asyncio.Semaphore] = {}
        self._last_refill: dict[str, float] = {}
    
    async def acquire(self, agent_id: str, tokens: int = 1) -> None:
        """Acquire rate limit tokens, blocking if necessary."""
        
        # Get or create semaphore for concurrent limit
        if agent_id not in self._semaphores:
            self._semaphores[agent_id] = asyncio.Semaphore(
                self.config.concurrent_requests
            )
        
        # Wait for concurrent slot
        await self._semaphores[agent_id].acquire()
        
        try:
            # Refill buckets
            self._refill(agent_id)
            
            # Wait for request tokens
            while self._request_tokens[agent_id] < 1:
                await asyncio.sleep(0.1)
                self._refill(agent_id)
            
            # Wait for token budget
            while self._token_tokens[agent_id] < tokens:
                await asyncio.sleep(0.1)
                self._refill(agent_id)
            
            # Consume tokens
            self._request_tokens[agent_id] -= 1
            self._token_tokens[agent_id] -= tokens
        except:
            self._semaphores[agent_id].release()
            raise
    
    def release(self, agent_id: str) -> None:
        """Release concurrent request slot."""
        if agent_id in self._semaphores:
            self._semaphores[agent_id].release()
    
    def _refill(self, agent_id: str) -> None:
        now = asyncio.get_event_loop().time()
        last = self._last_refill.get(agent_id, now)
        elapsed = now - last
        
        # Refill at rate per second
        request_refill = elapsed * (self.config.requests_per_minute / 60)
        token_refill = elapsed * (self.config.tokens_per_minute / 60)
        
        self._request_tokens[agent_id] = min(
            self.config.requests_per_minute,
            self._request_tokens[agent_id] + request_refill
        )
        self._token_tokens[agent_id] = min(
            self.config.tokens_per_minute,
            self._token_tokens[agent_id] + token_refill
        )
        
        self._last_refill[agent_id] = now
```

### Integration with Agent Backend

```python
class BudgetAwareBackend:
    """Wraps an agent backend with budget enforcement."""
    
    def __init__(
        self, 
        backend: AgentBackend, 
        budget_manager: BudgetManager,
        rate_limiter: RateLimiter
    ):
        self.backend = backend
        self.budget = budget_manager
        self.rate_limiter = rate_limiter
        self.agent_id: str | None = None
    
    async def initialize(self, config: AgentConfig) -> None:
        self.agent_id = config.name
        
        # Check if paused
        state = await self.budget._get_state(self.agent_id)
        if state.is_paused:
            raise AgentPausedError(state.pause_reason)
        
        # Check for model degradation
        if state.degraded_model:
            config.backend_config["model"] = state.degraded_model
        
        await self.backend.initialize(config)
    
    async def send(self, message: str) -> AsyncIterator[AgentMessage]:
        # Estimate tokens (rough)
        estimated_tokens = len(message) // 4 + 500  # Input + expected output
        
        # Acquire rate limit
        await self.rate_limiter.acquire(self.agent_id, estimated_tokens)
        
        try:
            total_input = 0
            total_output = 0
            
            async for msg in self.backend.send(message):
                # Track tokens
                if hasattr(msg, "usage"):
                    total_input = msg.usage.get("input_tokens", 0)
                    total_output = msg.usage.get("output_tokens", 0)
                
                yield msg
            
            # Record usage
            await self.budget.record_usage(
                agent_id=self.agent_id,
                model=self.backend.model,
                input_tokens=total_input,
                output_tokens=total_output,
                operation="query"
            )
        finally:
            self.rate_limiter.release(self.agent_id)
```

### Configuration

```yaml
# xpressai.yaml

system:
  budget:
    daily: $10.00
    monthly: $100.00
    on_exceeded: pause  # pause | alert | degrade | stop
    fallback_model: local  # For "degrade" action
    warn_at_percent: 80
  
  rate_limit:
    requests_per_minute: 60
    tokens_per_minute: 100000
    concurrent_requests: 5

agents:
  - name: atlas
    budget:
      daily: $5.00  # Override system default
      per_task: $1.00
```

### CLI Commands

```bash
# View budget status
xpressai budget

Agent: atlas
  Daily:   $3.45 / $5.00 (69%)  ████████░░ resets in 4h
  Monthly: $45.00 / $100.00 (45%)  ████░░░░░░ resets in 12d
  Status:  Active

# View detailed usage
xpressai budget atlas --detail

Date       Model           Tokens      Cost
2024-01-15 claude-sonnet   45,230      $0.82
2024-01-15 claude-sonnet   12,450      $0.23
2024-01-15 local           89,000      $0.00
...

# Resume a paused agent
xpressai budget atlas --resume

# Increase daily limit
xpressai budget atlas --set-daily 10.00
```

## Consequences

### Positive
- Prevents runaway spending
- Clear visibility into costs
- Flexible policies (pause, alert, degrade, stop)
- Per-agent and global limits
- Automatic fallback to local models

### Negative
- Adds latency for rate limit checks
- Token counting is approximate before API call
- Pricing data must be kept current
- Paused agents need manual intervention

### Implementation Notes

1. Start with cost tracking and basic limits
2. Add rate limiting
3. Implement "degrade" policy with model fallback
4. Add CLI budget commands
5. Show budget in TUI/Web dashboards

## Related ADRs
- ADR-002: Agent Backend (wrapped with budget)
- ADR-006: SQLite Storage (usage logs)
- ADR-011: Default Local Model (zero cost)
