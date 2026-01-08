"""Shared dependencies for web routes.

Contains the runtime reference, templates, and shared utility functions.
"""

from __future__ import annotations

import html as html_module
import logging
import re
from pathlib import Path
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from xpressai.core.runtime import Runtime

try:
    from fastapi import Request
    from fastapi.templating import Jinja2Templates
    from pydantic import BaseModel

    FASTAPI_AVAILABLE = True
except ImportError:
    FASTAPI_AVAILABLE = False
    BaseModel = object  # type: ignore

logger = logging.getLogger(__name__)

# Global runtime reference (set when app is created)
_runtime: Runtime | None = None


def get_runtime() -> Runtime | None:
    """Get the current runtime instance."""
    return _runtime


def set_runtime(runtime: Runtime | None) -> None:
    """Set the global runtime instance."""
    global _runtime
    _runtime = runtime


# Templates instance (initialized in create_app)
_templates: Jinja2Templates | None = None


def get_templates() -> Jinja2Templates | None:
    """Get the templates instance."""
    return _templates


def set_templates(templates: Jinja2Templates) -> None:
    """Set the templates instance."""
    global _templates
    _templates = templates


def render_markdown(text: str) -> str:
    """Simple markdown rendering without external dependencies."""
    # Escape HTML first for security
    text = html_module.escape(text)

    # Code blocks (``` ... ```)
    text = re.sub(
        r'```(\w*)\n(.*?)```',
        lambda m: f'<pre><code class="language-{m.group(1)}">{m.group(2)}</code></pre>',
        text,
        flags=re.DOTALL
    )

    # Inline code
    text = re.sub(r'`([^`]+)`', r'<code>\1</code>', text)

    # Headers
    text = re.sub(r'^### (.+)$', r'<h3>\1</h3>', text, flags=re.MULTILINE)
    text = re.sub(r'^## (.+)$', r'<h2>\1</h2>', text, flags=re.MULTILINE)
    text = re.sub(r'^# (.+)$', r'<h1>\1</h1>', text, flags=re.MULTILINE)

    # Bold and italic
    text = re.sub(r'\*\*\*(.+?)\*\*\*', r'<strong><em>\1</em></strong>', text)
    text = re.sub(r'\*\*(.+?)\*\*', r'<strong>\1</strong>', text)
    text = re.sub(r'\*(.+?)\*', r'<em>\1</em>', text)

    # Links
    text = re.sub(r'\[([^\]]+)\]\(([^)]+)\)', r'<a href="\2" target="_blank">\1</a>', text)

    # Horizontal rules
    text = re.sub(r'^---+$', r'<hr>', text, flags=re.MULTILINE)

    # Lists (simple)
    text = re.sub(r'^(\d+)\. (.+)$', r'<li>\2</li>', text, flags=re.MULTILINE)
    text = re.sub(r'^- (.+)$', r'<li>\1</li>', text, flags=re.MULTILINE)

    # Tables (basic support)
    lines = text.split('\n')
    in_table = False
    result_lines = []
    for line in lines:
        if '|' in line and not line.strip().startswith('<'):
            cells = [c.strip() for c in line.split('|')[1:-1]]
            if cells:
                if all(c.replace('-', '') == '' for c in cells):
                    continue  # Skip separator row
                if not in_table:
                    result_lines.append('<table>')
                    in_table = True
                result_lines.append('<tr>' + ''.join(f'<td>{c}</td>' for c in cells) + '</tr>')
            else:
                result_lines.append(line)
        else:
            if in_table:
                result_lines.append('</table>')
                in_table = False
            result_lines.append(line)
    if in_table:
        result_lines.append('</table>')
    text = '\n'.join(result_lines)

    # Paragraphs - convert remaining newlines
    text = re.sub(r'\n\n+', '</p><p>', text)
    text = text.replace('\n', '<br>')

    return text


# Request body models
class CreateTaskRequest(BaseModel):
    """Request body for creating a task."""
    title: str
    description: Optional[str] = None
    agent_id: Optional[str] = None


class AddMessageRequest(BaseModel):
    """Request body for adding a message to a task."""
    content: str
