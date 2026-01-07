"""Tests for the budget module."""

import pytest
from decimal import Decimal

from xpressai.budget.tracker import (
    UsageRecord,
    CostCalculator,
    ModelPricing,
)


class TestUsageRecord:
    """Tests for UsageRecord dataclass."""

    def test_create_usage_record(self):
        """Test creating a usage record."""
        from datetime import datetime

        record = UsageRecord(
            id=1,
            agent_id="atlas",
            timestamp=datetime.now(),
            model="claude-sonnet-4-20250514",
            input_tokens=100,
            output_tokens=50,
            cost_usd=Decimal("0.01"),
            operation="chat",
        )

        assert record.agent_id == "atlas"
        assert record.input_tokens == 100


class TestCostCalculator:
    """Tests for CostCalculator."""

    def test_claude_sonnet_pricing(self):
        """Test Claude Sonnet pricing calculation."""
        calculator = CostCalculator()

        cost = calculator.calculate("claude-sonnet-4-20250514", 1000, 500)

        # Input: 1000 * $3.00/1M = $0.003
        # Output: 500 * $15.00/1M = $0.0075
        # Total: $0.0105
        assert abs(float(cost) - 0.0105) < 0.0001

    def test_local_model_is_free(self):
        """Test local models have zero cost."""
        calculator = CostCalculator()

        cost = calculator.calculate("qwen3:8b", 10000, 5000)

        assert cost == Decimal("0")

    def test_unknown_model_defaults_to_haiku_pricing(self):
        """Test unknown models default to Haiku 4.5 pricing (not free)."""
        calculator = CostCalculator()

        # Unknown models use DEFAULT_PRICING (Haiku 4.5: $1/1M input, $5/1M output)
        cost = calculator.calculate("unknown-model", 1_000_000, 1_000_000)
        # 1M * $1/1M + 1M * $5/1M = $6
        assert cost == Decimal("6")

    def test_model_pricing_lookup(self):
        """Test getting model pricing."""
        calculator = CostCalculator()

        pricing = calculator.get_model_pricing("claude-sonnet-4-20250514")
        assert isinstance(pricing, ModelPricing)
        assert pricing.input == 3.00
        assert pricing.output == 15.00
        assert pricing.cache_write == 3.75
        assert pricing.cache_read == 0.30

    def test_list_models(self):
        """Test listing available models."""
        calculator = CostCalculator()

        models = calculator.list_models()
        assert "claude-sonnet-4-20250514" in models
        assert "qwen3:8b" in models

    def test_custom_pricing(self):
        """Test custom pricing override."""
        custom = {"my-model": ModelPricing(input=1.0, output=2.0)}
        calculator = CostCalculator(custom_pricing=custom)

        cost = calculator.calculate("my-model", 1_000_000, 1_000_000)
        # 1M * $1/1M + 1M * $2/1M = $3
        assert cost == Decimal("3")

    def test_estimate_cost(self):
        """Test cost estimation from text."""
        calculator = CostCalculator()

        # 100 chars = ~25 tokens input
        # With 1.5 ratio = ~37 tokens output
        text = "a" * 100
        cost = calculator.estimate_cost("qwen3:8b", text)

        # Local model is free
        assert cost == Decimal("0")


class TestCacheTokenPricing:
    """Tests for cache token pricing (prompt caching)."""

    def test_cache_tokens_included_in_calculation(self):
        """Test that cache tokens are included in cost calculation."""
        calculator = CostCalculator()

        # Claude Opus 4.5: input=$5, output=$25, cache_write=$6.25, cache_read=$0.50
        cost = calculator.calculate(
            model="claude-opus-4-5-20251101",
            input_tokens=1_000_000,  # $5.00
            output_tokens=1_000_000,  # $25.00
            cache_creation_tokens=1_000_000,  # $6.25
            cache_read_tokens=1_000_000,  # $0.50
        )

        # Total: $5 + $25 + $6.25 + $0.50 = $36.75
        assert cost == Decimal("36.75")

    def test_cache_tokens_zero_by_default(self):
        """Test that cache tokens default to zero."""
        calculator = CostCalculator()

        # Without cache tokens
        cost_without_cache = calculator.calculate(
            model="claude-sonnet-4-20250514",
            input_tokens=1_000_000,
            output_tokens=500_000,
        )

        # Same but explicitly passing zero cache
        cost_with_zero_cache = calculator.calculate(
            model="claude-sonnet-4-20250514",
            input_tokens=1_000_000,
            output_tokens=500_000,
            cache_creation_tokens=0,
            cache_read_tokens=0,
        )

        assert cost_without_cache == cost_with_zero_cache

    def test_cache_read_cheaper_than_input(self):
        """Test that cache read is cheaper than regular input."""
        calculator = CostCalculator()

        # Cost with regular input
        cost_input = calculator.calculate(
            model="claude-sonnet-4-20250514",
            input_tokens=1_000_000,
            output_tokens=0,
        )

        # Cost with cache read (same token count)
        cost_cache_read = calculator.calculate(
            model="claude-sonnet-4-20250514",
            input_tokens=0,
            output_tokens=0,
            cache_read_tokens=1_000_000,
        )

        # Cache read should be much cheaper
        assert cost_cache_read < cost_input
        # Sonnet 4: input=$3, cache_read=$0.30 (10x cheaper)
        assert cost_input == Decimal("3")
        assert cost_cache_read == Decimal("0.30")

    def test_detailed_calculation_includes_cache(self):
        """Test calculate_detailed includes cache token breakdown."""
        calculator = CostCalculator()

        details = calculator.calculate_detailed(
            model="claude-opus-4-5-20251101",
            input_tokens=1000,
            output_tokens=500,
            cache_creation_tokens=2000,
            cache_read_tokens=5000,
        )

        # Keys use cache_write/cache_read naming
        assert "cache_write" in details
        assert "cache_read" in details
        assert details["cache_write"] == Decimal("0.0125")  # 2000 * 6.25/1M
        assert details["cache_read"] == Decimal("0.0025")  # 5000 * 0.50/1M

    def test_local_models_have_no_cache_cost(self):
        """Test local models don't charge for cache tokens."""
        calculator = CostCalculator()

        cost = calculator.calculate(
            model="qwen3:8b",
            input_tokens=1_000_000,
            output_tokens=1_000_000,
            cache_creation_tokens=1_000_000,
            cache_read_tokens=1_000_000,
        )

        assert cost == Decimal("0")

    def test_gpt_models_cache_pricing(self):
        """Test GPT model cache pricing (read-only caching)."""
        calculator = CostCalculator()

        # GPT 5.2: input=$1.75, output=$14, cache_read=$0.175, no cache_write
        cost = calculator.calculate(
            model="gpt-5.2",
            input_tokens=1_000_000,
            output_tokens=0,
            cache_read_tokens=1_000_000,
        )

        # $1.75 input + $0.175 cache read = $1.925
        assert cost == Decimal("1.925")
