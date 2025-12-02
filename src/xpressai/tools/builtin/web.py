"""Web tools for XpressAI agents.

Provides web operations like:
- Fetching URLs
- Basic web browsing
- HTTP requests
"""

from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass
from typing import Any, Dict, Optional, TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.tools.registry import ToolRegistry

logger = logging.getLogger(__name__)

# Try to import aiohttp
try:
    import aiohttp

    AIOHTTP_AVAILABLE = True
except ImportError:
    AIOHTTP_AVAILABLE = False


@dataclass
class FetchResult:
    """Result of a URL fetch."""

    url: str
    status_code: int
    content: str
    content_type: str
    headers: Dict[str, str]
    error: Optional[str] = None


async def fetch_url(
    url: str,
    method: str = "GET",
    headers: Optional[Dict[str, str]] = None,
    timeout: int = 30,
    max_size: int = 1024 * 1024,  # 1MB default
) -> FetchResult:
    """Fetch content from a URL.

    Args:
        url: The URL to fetch
        method: HTTP method (GET, POST, etc.)
        headers: Optional headers to send
        timeout: Timeout in seconds
        max_size: Maximum response size in bytes

    Returns:
        FetchResult with response data
    """
    if not AIOHTTP_AVAILABLE:
        return FetchResult(
            url=url,
            status_code=0,
            content="",
            content_type="",
            headers={},
            error="aiohttp is not installed. Install with: pip install aiohttp",
        )

    logger.info(f"Fetching URL: {url}")

    try:
        async with aiohttp.ClientSession() as session:
            async with session.request(
                method,
                url,
                headers=headers,
                timeout=aiohttp.ClientTimeout(total=timeout),
            ) as response:
                # Check content length
                content_length = response.headers.get("Content-Length", "0")
                if int(content_length) > max_size:
                    return FetchResult(
                        url=url,
                        status_code=response.status,
                        content="",
                        content_type=response.content_type or "",
                        headers=dict(response.headers),
                        error=f"Response too large: {content_length} bytes",
                    )

                # Read content with size limit
                content = await response.text()
                if len(content) > max_size:
                    content = content[:max_size] + "\n... [truncated]"

                return FetchResult(
                    url=url,
                    status_code=response.status,
                    content=content,
                    content_type=response.content_type or "",
                    headers=dict(response.headers),
                )

    except asyncio.TimeoutError:
        return FetchResult(
            url=url,
            status_code=0,
            content="",
            content_type="",
            headers={},
            error=f"Request timed out after {timeout} seconds",
        )
    except Exception as e:
        logger.error(f"Fetch failed: {e}")
        return FetchResult(
            url=url, status_code=0, content="", content_type="", headers={}, error=str(e)
        )


async def register_web_tools(registry: ToolRegistry) -> None:
    """Register web tools with the registry.

    Args:
        registry: The tool registry
    """
    from xpressai.tools.registry import ToolDefinition, ToolCategory

    async def fetch_wrapper(
        url: str,
        method: str = "GET",
        headers: Optional[Dict[str, str]] = None,
        timeout: int = 30,
    ) -> Dict[str, Any]:
        """Wrapper that returns dict for MCP."""
        result = await fetch_url(
            url=url,
            method=method,
            headers=headers,
            timeout=timeout,
        )
        return {
            "url": result.url,
            "status_code": result.status_code,
            "content": result.content,
            "content_type": result.content_type,
            "headers": result.headers,
            "error": result.error,
        }

    registry.register_tool(
        ToolDefinition(
            name="fetch_url",
            description="Fetch content from a URL. Returns the response body and metadata.",
            category=ToolCategory.WEB,
            input_schema={
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "The URL to fetch"},
                    "method": {
                        "type": "string",
                        "description": "HTTP method (GET, POST, etc.)",
                        "default": "GET",
                    },
                    "headers": {"type": "object", "description": "Optional headers to send"},
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds",
                        "default": 30,
                    },
                },
                "required": ["url"],
            },
            handler=fetch_wrapper,
        )
    )
