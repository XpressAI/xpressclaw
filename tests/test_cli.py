"""Tests for CLI commands."""

import pytest
from pathlib import Path
from click.testing import CliRunner
from unittest.mock import patch, MagicMock, AsyncMock

from xpressai.cli.main import cli


@pytest.fixture
def runner():
    """Create a CLI runner."""
    return CliRunner()


@pytest.fixture
def mock_runtime():
    """Create a mock runtime."""
    runtime = MagicMock()
    runtime.is_running = True
    runtime.is_initialized = True
    runtime.memory_manager = MagicMock()
    runtime.task_board = MagicMock()

    # Mock async methods
    runtime.initialize = AsyncMock()
    runtime.list_agents = AsyncMock(return_value=[])
    runtime.get_budget_summary = AsyncMock(return_value={"total_spent": 0})
    runtime.get_task_counts = AsyncMock(return_value={"pending": 0, "in_progress": 0, "completed": 0})

    return runtime


class TestCliHelp:
    """Test CLI help commands."""

    def test_main_help(self, runner):
        """Test main help shows all commands."""
        result = runner.invoke(cli, ["--help"])
        assert result.exit_code == 0
        assert "memory" in result.output
        assert "dashboard" in result.output
        assert "status" in result.output

    def test_memory_help(self, runner):
        """Test memory subcommand help."""
        result = runner.invoke(cli, ["memory", "--help"])
        assert result.exit_code == 0
        assert "list" in result.output
        assert "search" in result.output
        assert "show" in result.output
        assert "stats" in result.output
        assert "slots" in result.output


class TestMemoryCommand:
    """Tests for memory CLI commands."""

    def test_memory_list_no_config(self, runner, temp_dir):
        """Test memory list without config file."""
        with runner.isolated_filesystem(temp_dir=temp_dir):
            result = runner.invoke(cli, ["memory", "list"])
            assert "No xpressai.yaml found" in result.output

    def test_memory_stats_no_config(self, runner, temp_dir):
        """Test memory stats without config file."""
        with runner.isolated_filesystem(temp_dir=temp_dir):
            result = runner.invoke(cli, ["memory", "stats"])
            assert "No xpressai.yaml found" in result.output

    def test_memory_search_no_config(self, runner, temp_dir):
        """Test memory search without config file."""
        with runner.isolated_filesystem(temp_dir=temp_dir):
            result = runner.invoke(cli, ["memory", "search", "test query"])
            assert "No xpressai.yaml found" in result.output

    def test_memory_stats_no_database(self, runner, config_file):
        """Test memory stats handles missing database gracefully."""
        result = runner.invoke(cli, ["memory", "stats"], catch_exceptions=False)
        # Should show user-friendly message, not crash
        assert result.exit_code == 0 or "No memories found" in result.output or "no such table" not in result.output.lower()


class TestStatusCommand:
    """Tests for status CLI command."""

    def test_status_no_config(self, runner, temp_dir):
        """Test status without config file."""
        with runner.isolated_filesystem(temp_dir=temp_dir):
            result = runner.invoke(cli, ["status"])
            assert "No xpressai.yaml found" in result.output

    @patch("xpressai.cli.status_cmd._get_daemon_status")
    def test_status_shows_daemon_status(self, mock_get_status, runner, sample_config, temp_dir):
        """Test status shows daemon info when connected."""
        import os
        import yaml

        # Create config in temp dir
        config_path = temp_dir / "xpressai.yaml"
        with open(config_path, "w") as f:
            yaml.dump(sample_config, f)

        mock_get_status.return_value = {
            "status": "running",
            "agents": [{"name": "atlas", "status": "running", "backend": "claude-code"}],
            "budget": {"total_spent": 1.50},
        }

        old_cwd = os.getcwd()
        os.chdir(temp_dir)
        try:
            result = runner.invoke(cli, ["status"])
            assert "connected to running daemon" in result.output
            assert "atlas" in result.output
        finally:
            os.chdir(old_cwd)


class TestDashboardCommand:
    """Tests for dashboard CLI command."""

    @patch("urllib.request.urlopen")
    def test_dashboard_detects_running_server(self, mock_urlopen, runner, temp_dir):
        """Test dashboard detects already running server."""
        # Mock successful health check
        mock_response = MagicMock()
        mock_response.__enter__ = MagicMock(return_value=mock_response)
        mock_response.__exit__ = MagicMock(return_value=False)
        mock_urlopen.return_value = mock_response

        with runner.isolated_filesystem(temp_dir=temp_dir):
            result = runner.invoke(cli, ["dashboard"])
            assert "available" in result.output.lower()
            assert "http://" in result.output

    @patch("urllib.request.urlopen")
    def test_dashboard_shows_instructions_when_no_runtime(self, mock_urlopen, runner, temp_dir):
        """Test dashboard shows instructions when no runtime running."""
        # Mock failed health check (no server running)
        mock_urlopen.side_effect = Exception("Connection refused")

        with runner.isolated_filesystem(temp_dir=temp_dir):
            result = runner.invoke(cli, ["dashboard"])
            assert "not available" in result.output.lower()
            assert "xpressai up" in result.output

    @patch("urllib.request.urlopen")
    @patch("webbrowser.open")
    def test_dashboard_opens_browser(self, mock_webbrowser, mock_urlopen, runner, temp_dir):
        """Test dashboard --open flag opens browser."""
        # Mock successful health check
        mock_response = MagicMock()
        mock_response.__enter__ = MagicMock(return_value=mock_response)
        mock_response.__exit__ = MagicMock(return_value=False)
        mock_urlopen.return_value = mock_response

        with runner.isolated_filesystem(temp_dir=temp_dir):
            result = runner.invoke(cli, ["dashboard", "--open"])
            assert mock_webbrowser.called
            assert "opened" in result.output.lower()


class TestTasksCommand:
    """Tests for tasks CLI commands."""

    def test_tasks_list_requires_agent(self, runner, temp_dir):
        """Test tasks command requires agent argument."""
        import os

        old_cwd = os.getcwd()
        os.chdir(temp_dir)
        try:
            result = runner.invoke(cli, ["tasks"])
            assert result.exit_code != 0 or "Missing argument" in result.output or "Usage" in result.output
        finally:
            os.chdir(old_cwd)

    def test_tasks_help(self, runner):
        """Test tasks subcommand help."""
        result = runner.invoke(cli, ["tasks", "agent", "--help"])
        assert result.exit_code == 0
        assert "list" in result.output
        assert "add" in result.output
        assert "schedule" in result.output


class TestSopCommand:
    """Tests for SOP CLI commands."""

    def test_sop_help(self, runner):
        """Test SOP subcommand help."""
        result = runner.invoke(cli, ["sop", "--help"])
        assert result.exit_code == 0
        assert "list" in result.output
        assert "show" in result.output
        assert "create" in result.output
        assert "delete" in result.output


class TestBudgetCommand:
    """Tests for budget CLI commands."""

    def test_budget_help(self, runner):
        """Test budget subcommand help."""
        result = runner.invoke(cli, ["budget", "--help"])
        assert result.exit_code == 0
        assert "show" in result.output


class TestUpCommand:
    """Tests for up command utilities."""

    def test_is_port_in_use_free_port(self):
        """Test port check with a free port."""
        from xpressai.cli.up_cmd import _is_port_in_use
        # Use a high port that's unlikely to be in use
        assert _is_port_in_use(59999) == False

    def test_is_port_in_use_bound_port(self):
        """Test port check with a bound port."""
        import socket
        from xpressai.cli.up_cmd import _is_port_in_use

        # Bind to a port temporarily
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind(("127.0.0.1", 59998))
        sock.listen(1)

        try:
            assert _is_port_in_use(59998) == True
        finally:
            sock.close()
