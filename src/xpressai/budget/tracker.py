"""Cost tracking for XpressAI.

Tracks token usage and calculates costs for different models.
"""

from dataclasses import dataclass
from datetime import datetime
from decimal import Decimal
from typing import Any


@dataclass
class UsageRecord:
    """A record of API usage.

    Attributes:
        id: Record ID
        agent_id: ID of the agent
        timestamp: When the usage occurred
        model: Model used
        input_tokens: Number of input tokens
        output_tokens: Number of output tokens
        cost_usd: Cost in USD
        operation: Type of operation
        session_id: Optional session ID
    """

    id: int
    agent_id: str
    timestamp: datetime
    model: str
    input_tokens: int
    output_tokens: int
    cost_usd: Decimal
    operation: str
    session_id: str | None = None


class CostCalculator:
    """Calculates costs for different models.

    Pricing is per 1M tokens, rates may change over time.
    """

    # Prices per 1M tokens as of 2024
    PRICING: dict[str, dict[str, float]] = {
        # Claude models
        "claude-sonnet-4-20250514": {"input": 3.00, "output": 15.00},
        "claude-haiku-3-20240307": {"input": 0.25, "output": 1.25},
        "claude-opus-4-20250514": {"input": 15.00, "output": 75.00},
        # OpenAI models
        "gpt-4o": {"input": 2.50, "output": 10.00},
        "gpt-4o-mini": {"input": 0.15, "output": 0.60},
        "gpt-4-turbo": {"input": 10.00, "output": 30.00},
        # Local models via vLLM (free - self-hosted)
        "local": {"input": 0.00, "output": 0.00},
        "Qwen/Qwen3-8B": {"input": 0.00, "output": 0.00},
        "Qwen/Qwen3-14B": {"input": 0.00, "output": 0.00},
        "Qwen/Qwen3-32B": {"input": 0.00, "output": 0.00},
        "meta-llama/Llama-3.1-8B-Instruct": {"input": 0.00, "output": 0.00},
        "meta-llama/Llama-3.1-70B-Instruct": {"input": 0.00, "output": 0.00},
        # Local models via Ollama (free - self-hosted)
        "qwen3:8b": {"input": 0.00, "output": 0.00},
        "qwen3:14b": {"input": 0.00, "output": 0.00},
        "llama3:8b": {"input": 0.00, "output": 0.00},
    }

    def __init__(self, custom_pricing: dict[str, dict[str, float]] | None = None):
        """Initialize calculator.

        Args:
            custom_pricing: Optional custom pricing overrides
        """
        self.pricing = {**self.PRICING}
        if custom_pricing:
            self.pricing.update(custom_pricing)

    def calculate(
        self,
        model: str,
        input_tokens: int,
        output_tokens: int,
    ) -> Decimal:
        """Calculate cost for token usage.

        Args:
            model: Model identifier
            input_tokens: Number of input tokens
            output_tokens: Number of output tokens

        Returns:
            Cost in USD
        """
        # Try exact match first, then prefix match
        pricing = self.pricing.get(model)

        if pricing is None:
            # Try prefix matching (e.g., "claude-sonnet" matches "claude-sonnet-4-...")
            for key in self.pricing:
                if model.startswith(key) or key.startswith(model):
                    pricing = self.pricing[key]
                    break

        if pricing is None:
            # Default to free for unknown models
            pricing = {"input": 0, "output": 0}

        input_cost = Decimal(str(pricing["input"])) * input_tokens / 1_000_000
        output_cost = Decimal(str(pricing["output"])) * output_tokens / 1_000_000

        return input_cost + output_cost

    def estimate_cost(
        self,
        model: str,
        text: str,
        expected_output_ratio: float = 1.5,
    ) -> Decimal:
        """Estimate cost for processing text.

        Uses a rough token estimate (4 chars per token).

        Args:
            model: Model identifier
            text: Input text
            expected_output_ratio: Expected output/input ratio

        Returns:
            Estimated cost in USD
        """
        # Rough estimate: ~4 characters per token
        input_tokens = len(text) // 4
        output_tokens = int(input_tokens * expected_output_ratio)

        return self.calculate(model, input_tokens, output_tokens)

    def get_model_pricing(self, model: str) -> dict[str, float]:
        """Get pricing for a model.

        Args:
            model: Model identifier

        Returns:
            Pricing dict with input/output rates
        """
        return self.pricing.get(model, {"input": 0, "output": 0})

    def list_models(self) -> list[str]:
        """List all models with pricing.

        Returns:
            List of model identifiers
        """
        return list(self.pricing.keys())
