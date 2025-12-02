"""Tests for the budget module."""

import pytest
from decimal import Decimal

from xpressai.budget.tracker import (
    UsageRecord,
    CostCalculator,
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

    def test_unknown_model_defaults_to_free(self):
        """Test unknown models default to free."""
        calculator = CostCalculator()

        cost = calculator.calculate("unknown-model", 100, 50)
        assert cost == Decimal("0")

    def test_model_pricing_lookup(self):
        """Test getting model pricing."""
        calculator = CostCalculator()

        pricing = calculator.get_model_pricing("claude-sonnet-4-20250514")
        assert pricing["input"] == 3.00
        assert pricing["output"] == 15.00

    def test_list_models(self):
        """Test listing available models."""
        calculator = CostCalculator()

        models = calculator.list_models()
        assert "claude-sonnet-4-20250514" in models
        assert "qwen3:8b" in models

    def test_custom_pricing(self):
        """Test custom pricing override."""
        custom = {"my-model": {"input": 1.0, "output": 2.0}}
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
