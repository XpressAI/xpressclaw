"""Built-in MCP tools for XpressAI.

Provides standard tools for:
- Filesystem operations (read, write, list, search)
- Shell command execution
- Web browsing and fetching
- User interaction (ask_user)
- Task control (complete_task, fail_task)
"""

from xpressai.tools.builtin.filesystem import register_filesystem_tools
from xpressai.tools.builtin.shell import register_shell_tools
from xpressai.tools.builtin.web import register_web_tools
from xpressai.tools.builtin.ask_user import (
    register_ask_user_tool,
    set_conversation_manager,
    current_task_id,
)
from xpressai.tools.builtin.task_control import (
    register_task_control_tools,
    reset_task_completion,
    is_task_completed,
    get_completion_summary,
)

__all__ = [
    "register_filesystem_tools",
    "register_shell_tools",
    "register_web_tools",
    "register_ask_user_tool",
    "set_conversation_manager",
    "current_task_id",
    "register_task_control_tools",
    "reset_task_completion",
    "is_task_completed",
    "get_completion_summary",
]
