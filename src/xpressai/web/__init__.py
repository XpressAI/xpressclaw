"""Web UI module for XpressAI.

Built with FastAPI + HTMX for a server-rendered, minimal JavaScript dashboard.
"""

from xpressai.web.app import create_app, run_web

__all__ = ["create_app", "run_web"]
