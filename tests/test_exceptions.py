"""Tests for the exceptions module."""

import pytest

from xpressai.core.exceptions import (
    XpressAIError,
    ConfigError,
    ConfigValidationError,
    AgentError,
    BudgetExceededError,
    IsolationError,
    MemoryError,
    ToolError,
)


class TestExceptionHierarchy:
    """Test the exception hierarchy."""

    def test_base_exception(self):
        """Test XpressAIError is the base."""
        err = XpressAIError("test error")
        assert str(err) == "test error"

    def test_all_inherit_from_base(self):
        """Test all exceptions inherit from XpressAIError."""
        exceptions = [
            ConfigError,
            AgentError,
            IsolationError,
            MemoryError,
            ToolError,
        ]

        for exc_class in exceptions:
            err = exc_class("test")
            assert isinstance(err, XpressAIError)

    def test_config_validation_error(self):
        """Test ConfigValidationError."""
        err = ConfigValidationError("Invalid config")
        assert "Invalid config" in str(err)

    def test_agent_error(self):
        """Test AgentError."""
        err = AgentError("Agent crashed")
        assert "Agent crashed" in str(err)

    def test_budget_exceeded_error(self):
        """Test BudgetExceededError with amounts."""
        err = BudgetExceededError(
            "Budget exceeded",
            agent_id="atlas",
            limit_type="daily",
            limit=20.0,
            current=25.0,
        )
        assert err.agent_id == "atlas"
        assert err.limit == 20.0
        assert err.current == 25.0
        assert err.limit_type == "daily"

    def test_exception_with_context(self):
        """Test exception with context dict."""
        err = XpressAIError("test", context={"key": "value"})
        assert "key=value" in str(err)

    def test_tool_error(self):
        """Test ToolError."""
        err = ToolError("Tool failed")
        assert "Tool failed" in str(err)
