"""Built-in MCP tools for XpressAI.

Provides standard tools for:
- Filesystem operations (read, write, list, search)
- Shell command execution
- Web browsing and fetching
"""

from xpressai.tools.builtin.filesystem import register_filesystem_tools
from xpressai.tools.builtin.shell import register_shell_tools
from xpressai.tools.builtin.web import register_web_tools

__all__ = [
    "register_filesystem_tools",
    "register_shell_tools",
    "register_web_tools",
]
