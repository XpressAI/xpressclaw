"""Cost tracking for XpressAI.

Tracks token usage and calculates costs for different models.
"""

from dataclasses import dataclass, field
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
        cache_creation_tokens: Tokens used to create cache
        cache_read_tokens: Tokens read from cache
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
    cache_creation_tokens: int = 0
    cache_read_tokens: int = 0
    session_id: str | None = None


@dataclass
class ModelPricing:
    """Pricing configuration for a model (per 1M tokens)."""

    input: float  # Base input price
    output: float  # Output price
    cache_write: float = 0.0  # Cache creation/write price (if different from input)
    cache_read: float = 0.0  # Cache hit/read price


class CostCalculator:
    """Calculates costs for different models.

    Pricing is per 1M tokens. Updated January 2026.
    """

    # Prices per 1M tokens
    PRICING: dict[str, ModelPricing] = {
        # Claude models (Anthropic) - January 2026
        # Claude Opus 4.5
        "claude-opus-4-5-20251101": ModelPricing(
            input=5.00, output=25.00, cache_write=6.25, cache_read=0.50
        ),
        # Claude Opus 4.1
        "claude-opus-4-1-20250414": ModelPricing(
            input=15.00, output=75.00, cache_write=18.75, cache_read=1.50
        ),
        # Claude Opus 4
        "claude-opus-4-20250514": ModelPricing(
            input=15.00, output=75.00, cache_write=18.75, cache_read=1.50
        ),
        # Claude Sonnet 4.5
        "claude-sonnet-4-5-20251022": ModelPricing(
            input=3.00, output=15.00, cache_write=3.75, cache_read=0.30
        ),
        # Claude Sonnet 4
        "claude-sonnet-4-20250514": ModelPricing(
            input=3.00, output=15.00, cache_write=3.75, cache_read=0.30
        ),
        # Claude Sonnet 3.7 (deprecated)
        "claude-3-7-sonnet-20250219": ModelPricing(
            input=3.00, output=15.00, cache_write=3.75, cache_read=0.30
        ),
        # Claude Haiku 4.5
        "claude-haiku-4-5-20251022": ModelPricing(
            input=1.00, output=5.00, cache_write=1.25, cache_read=0.10
        ),
        # Claude Haiku 3.5
        "claude-3-5-haiku-20241022": ModelPricing(
            input=0.80, output=4.00, cache_write=1.00, cache_read=0.08
        ),
        # Claude Opus 3 (deprecated)
        "claude-3-opus-20240229": ModelPricing(
            input=15.00, output=75.00, cache_write=18.75, cache_read=1.50
        ),
        # Claude Haiku 3
        "claude-3-haiku-20240307": ModelPricing(
            input=0.25, output=1.25, cache_write=0.30, cache_read=0.03
        ),
        # OpenAI/GPT models - January 2026
        # GPT-5.2
        "gpt-5.2": ModelPricing(input=1.75, output=14.00, cache_read=0.175),
        # GPT-5.2 pro
        "gpt-5.2-pro": ModelPricing(input=21.00, output=168.00),
        # GPT-5 mini
        "gpt-5-mini": ModelPricing(input=0.25, output=2.00, cache_read=0.025),
        # GPT-4o (legacy)
        "gpt-4o": ModelPricing(input=2.50, output=10.00, cache_read=1.25),
        "gpt-4o-mini": ModelPricing(input=0.15, output=0.60, cache_read=0.075),
        "gpt-4-turbo": ModelPricing(input=10.00, output=30.00),
        # Open source models (hosted)
        "gpt-oss-120b": ModelPricing(input=0.22, output=0.59),
        # Local models via vLLM (free - self-hosted)
        "local": ModelPricing(input=0.00, output=0.00),
        "Qwen/Qwen3-8B": ModelPricing(input=0.00, output=0.00),
        "Qwen/Qwen3-14B": ModelPricing(input=0.00, output=0.00),
        "Qwen/Qwen3-32B": ModelPricing(input=0.00, output=0.00),
        "meta-llama/Llama-3.1-8B-Instruct": ModelPricing(input=0.00, output=0.00),
        "meta-llama/Llama-3.1-70B-Instruct": ModelPricing(input=0.00, output=0.00),
        # Local models via Ollama (free - self-hosted)
        "qwen3:8b": ModelPricing(input=0.00, output=0.00),
        "qwen3:14b": ModelPricing(input=0.00, output=0.00),
        "llama3:8b": ModelPricing(input=0.00, output=0.00),
    }

    # Aliases for common model name patterns
    MODEL_ALIASES: dict[str, str] = {
        "claude-opus-4.5": "claude-opus-4-5-20251101",
        "claude-opus-4.1": "claude-opus-4-1-20250414",
        "claude-opus-4": "claude-opus-4-20250514",
        "claude-sonnet-4.5": "claude-sonnet-4-5-20251022",
        "claude-sonnet-4": "claude-sonnet-4-20250514",
        "claude-haiku-4.5": "claude-haiku-4-5-20251022",
        "claude-haiku-3.5": "claude-3-5-haiku-20241022",
        "claude-haiku-3": "claude-3-haiku-20240307",
        "claude-opus-3": "claude-3-opus-20240229",
    }

    def __init__(self, custom_pricing: dict[str, ModelPricing] | None = None):
        """Initialize calculator.

        Args:
            custom_pricing: Optional custom pricing overrides
        """
        self.pricing = {**self.PRICING}
        if custom_pricing:
            self.pricing.update(custom_pricing)

    def register_pricing(self, model: str, price_input: float, price_output: float) -> None:
        """Register custom pricing for a model.

        Args:
            model: Model identifier
            price_input: Price per 1M input tokens
            price_output: Price per 1M output tokens
        """
        self.pricing[model] = ModelPricing(input=price_input, output=price_output)

    def _resolve_model(self, model: str) -> str:
        """Resolve a model name to its canonical form.

        Args:
            model: Model identifier or alias

        Returns:
            Canonical model name
        """
        # Check aliases first
        if model in self.MODEL_ALIASES:
            return self.MODEL_ALIASES[model]

        # Try exact match
        if model in self.pricing:
            return model

        # Try prefix matching
        for key in self.pricing:
            if model.startswith(key) or key.startswith(model):
                return key

        return model

    # Default pricing for unknown models (use Haiku 4.5 rates)
    DEFAULT_PRICING = ModelPricing(input=1.00, output=5.00, cache_write=1.25, cache_read=0.10)

    def _get_pricing(self, model: str) -> ModelPricing:
        """Get pricing for a model.

        Uses Claude Haiku 3.5 pricing as default for unknown models.

        Args:
            model: Model identifier

        Returns:
            ModelPricing instance
        """
        resolved = self._resolve_model(model)
        return self.pricing.get(resolved, self.DEFAULT_PRICING)

    def calculate(
        self,
        model: str,
        input_tokens: int,
        output_tokens: int,
        cache_creation_tokens: int = 0,
        cache_read_tokens: int = 0,
    ) -> Decimal:
        """Calculate cost for token usage.

        Args:
            model: Model identifier
            input_tokens: Number of input tokens (non-cached)
            output_tokens: Number of output tokens
            cache_creation_tokens: Tokens written to cache
            cache_read_tokens: Tokens read from cache (cache hits)

        Returns:
            Cost in USD
        """
        pricing = self._get_pricing(model)

        # Calculate each component
        input_cost = Decimal(str(pricing.input)) * input_tokens / 1_000_000
        output_cost = Decimal(str(pricing.output)) * output_tokens / 1_000_000

        # Cache costs
        cache_write_cost = Decimal("0")
        if cache_creation_tokens > 0:
            # Use cache_write price, or fallback to input price if not set
            cache_price = pricing.cache_write if pricing.cache_write > 0 else pricing.input
            cache_write_cost = Decimal(str(cache_price)) * cache_creation_tokens / 1_000_000

        cache_read_cost = Decimal("0")
        if cache_read_tokens > 0 and pricing.cache_read > 0:
            cache_read_cost = Decimal(str(pricing.cache_read)) * cache_read_tokens / 1_000_000

        return input_cost + output_cost + cache_write_cost + cache_read_cost

    def calculate_detailed(
        self,
        model: str,
        input_tokens: int,
        output_tokens: int,
        cache_creation_tokens: int = 0,
        cache_read_tokens: int = 0,
    ) -> dict[str, Decimal]:
        """Calculate cost with breakdown by category.

        Args:
            model: Model identifier
            input_tokens: Number of input tokens
            output_tokens: Number of output tokens
            cache_creation_tokens: Tokens written to cache
            cache_read_tokens: Tokens read from cache

        Returns:
            Dict with cost breakdown: input, output, cache_write, cache_read, total
        """
        pricing = self._get_pricing(model)

        input_cost = Decimal(str(pricing.input)) * input_tokens / 1_000_000
        output_cost = Decimal(str(pricing.output)) * output_tokens / 1_000_000

        cache_write_cost = Decimal("0")
        if cache_creation_tokens > 0:
            cache_price = pricing.cache_write if pricing.cache_write > 0 else pricing.input
            cache_write_cost = Decimal(str(cache_price)) * cache_creation_tokens / 1_000_000

        cache_read_cost = Decimal("0")
        if cache_read_tokens > 0 and pricing.cache_read > 0:
            cache_read_cost = Decimal(str(pricing.cache_read)) * cache_read_tokens / 1_000_000

        total = input_cost + output_cost + cache_write_cost + cache_read_cost

        return {
            "input": input_cost,
            "output": output_cost,
            "cache_write": cache_write_cost,
            "cache_read": cache_read_cost,
            "total": total,
        }

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

    def get_model_pricing(self, model: str) -> ModelPricing:
        """Get pricing for a model.

        Args:
            model: Model identifier

        Returns:
            ModelPricing instance
        """
        return self._get_pricing(model)

    def list_models(self) -> list[str]:
        """List all models with pricing.

        Returns:
            List of model identifiers
        """
        return list(self.pricing.keys())

    def format_cost_report(
        self,
        model: str,
        input_tokens: int,
        output_tokens: int,
        cache_creation_tokens: int = 0,
        cache_read_tokens: int = 0,
    ) -> str:
        """Format a human-readable cost report.

        Args:
            model: Model identifier
            input_tokens: Number of input tokens
            output_tokens: Number of output tokens
            cache_creation_tokens: Tokens written to cache
            cache_read_tokens: Tokens read from cache

        Returns:
            Formatted cost report string
        """
        breakdown = self.calculate_detailed(
            model, input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens
        )
        pricing = self._get_pricing(model)

        lines = [
            f"Model: {model}",
            f"  Input: {input_tokens:,} tokens × ${pricing.input}/MTok = ${breakdown['input']:.6f}",
            f"  Output: {output_tokens:,} tokens × ${pricing.output}/MTok = ${breakdown['output']:.6f}",
        ]

        if cache_creation_tokens > 0:
            cache_price = pricing.cache_write if pricing.cache_write > 0 else pricing.input
            lines.append(
                f"  Cache Write: {cache_creation_tokens:,} tokens × ${cache_price}/MTok = ${breakdown['cache_write']:.6f}"
            )

        if cache_read_tokens > 0:
            lines.append(
                f"  Cache Read: {cache_read_tokens:,} tokens × ${pricing.cache_read}/MTok = ${breakdown['cache_read']:.6f}"
            )

        lines.append(f"  Total: ${breakdown['total']:.6f}")

        return "\n".join(lines)
