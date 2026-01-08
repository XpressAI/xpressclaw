"""Memory management routes.

Contains page routes, API routes, and HTMX partials for memory system.
"""

from __future__ import annotations

import html as html_module
import logging

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse

from xpressai.web.deps import get_runtime, get_templates

logger = logging.getLogger(__name__)

router = APIRouter()


# -------------------------
# Page Routes
# -------------------------

@router.get("/memory", response_class=HTMLResponse)
async def memory_page(request: Request):
    """Memory page (zettelkasten browser)."""
    runtime = get_runtime()
    templates = get_templates()

    agents = []
    if runtime:
        agents = await runtime.list_agents()
    if templates:
        return templates.TemplateResponse(
            "zettelkasten.html", {"request": request, "active": "memory", "agents": agents}
        )
    return HTMLResponse("<h1>Memory - Templates not installed</h1>")


@router.get("/zettelkasten", response_class=HTMLResponse)
async def zettelkasten_page(request: Request):
    """Zettelkasten browser page (alias for /memory)."""
    runtime = get_runtime()
    templates = get_templates()

    agents = []
    if runtime:
        agents = await runtime.list_agents()
    if templates:
        return templates.TemplateResponse(
            "zettelkasten.html", {"request": request, "active": "zettelkasten", "agents": agents}
        )
    return HTMLResponse("<h1>Zettelkasten - Templates not installed</h1>")


# -------------------------
# API Routes
# -------------------------

@router.get("/api/memory/stats")
async def get_memory_stats():
    """Get memory system stats."""
    runtime = get_runtime()
    if not runtime or not runtime.memory_manager:
        return {"error": "Memory not available"}

    return await runtime.memory_manager.get_stats()


@router.delete("/api/memory/{memory_id}")
async def delete_memory(memory_id: str):
    """Delete a memory by ID."""
    runtime = get_runtime()
    if not runtime or not runtime.memory_manager:
        return {"error": "Memory not available"}

    try:
        await runtime.memory_manager.delete(memory_id)
        return {"status": "ok", "deleted": memory_id}
    except Exception as e:
        logger.warning(f"Failed to delete memory {memory_id}: {e}")
        return {"error": str(e)}


# -------------------------
# HTMX Partials
# -------------------------

@router.get("/partials/memory", response_class=HTMLResponse)
async def memory_partial(request: Request):
    """HTMX partial for memory stats (dashboard)."""
    runtime = get_runtime()
    if not runtime or not runtime.memory_manager:
        return HTMLResponse('<div class="empty-state">Memory not available</div>')

    stats = await runtime.memory_manager.get_stats()
    zettel = stats.get("zettelkasten", {})
    total = zettel.get("total_memories", 0)
    links = zettel.get("total_links", 0)

    return HTMLResponse(f"""
        <div class="stats-row">
            <div class="stat-item">
                <span class="stat-value">{total}</span>
                <span class="stat-label">Memories</span>
            </div>
            <div class="stat-item">
                <span class="stat-value">{links}</span>
                <span class="stat-label">Links</span>
            </div>
        </div>
    """)


@router.get("/partials/memory/stats", response_class=HTMLResponse)
async def memory_stats_partial(request: Request, agent: str = ""):
    """HTMX partial for memory stats (memory page)."""
    runtime = get_runtime()
    if not runtime or not runtime.memory_manager:
        return HTMLResponse('<div class="empty-state">Memory not available</div>')

    # Get count for this agent
    agent_id = agent if agent else None
    memories = await runtime.memory_manager.get_recent(agent_id=agent_id, limit=1000)
    total = len(memories)

    return HTMLResponse(f"""
        <div class="stats-row">
            <div class="stat-item">
                <span class="stat-value">{total}</span>
                <span class="stat-label">Memories</span>
            </div>
        </div>
    """)


@router.get("/partials/memory/recent", response_class=HTMLResponse)
async def memory_recent_partial(request: Request, agent: str = ""):
    """HTMX partial for recent memories."""
    runtime = get_runtime()
    if not runtime or not runtime.memory_manager:
        return HTMLResponse('<div class="empty-state">Memory not available</div>')

    agent_id = agent if agent else None
    memories = await runtime.memory_manager.get_recent(agent_id=agent_id, limit=20)

    if not memories:
        return HTMLResponse('<div class="empty-state">No memories yet</div>')

    html_parts = ['<div class="memory-list">']
    for memory in memories:
        tags_html = ""
        if memory.tags:
            tags_html = '<div class="tags">' + "".join(
                f'<span class="tag">#{t}</span>' for t in memory.tags[:3]
            ) + "</div>"

        # Escape content for HTML - handle None values safely
        content_text = memory.content if memory.content else "(no content)"
        summary_text = memory.summary if memory.summary else content_text[:100]
        safe_summary = html_module.escape(summary_text)
        safe_content = html_module.escape(content_text)
        truncated = len(summary_text) > 80
        display_summary = safe_summary[:80] + ('...' if truncated else '')

        html_parts.append(f"""
            <div class="memory-item" data-memory-id="{memory.id}">
                <div class="memory-header" onclick="toggleMemory(this)">
                    <div class="summary">{display_summary}</div>
                    <div class="memory-actions">
                        <button class="btn-outline-danger"
                                onclick="event.stopPropagation(); deleteMemory('{memory.id}')"
                                title="Delete memory">×</button>
                    </div>
                </div>
                <div class="memory-details" style="display: none;">
                    <div class="memory-content">{safe_content}</div>
                    <div class="meta">
                        <span class="layer">{memory.layer}</span>
                        <span class="date">{memory.created_at.strftime('%Y-%m-%d %H:%M')}</span>
                        <span class="id">ID: {memory.id[:8]}...</span>
                    </div>
                    {tags_html}
                </div>
            </div>
        """)

    html_parts.append("</div>")
    return HTMLResponse("".join(html_parts))


@router.get("/partials/memory/search", response_class=HTMLResponse)
async def memory_search_partial(request: Request, q: str = "", agent: str = ""):
    """HTMX partial for memory search results."""
    runtime = get_runtime()
    if not runtime or not runtime.memory_manager:
        return HTMLResponse('<div class="empty-state">Memory not available</div>')

    if not q:
        return HTMLResponse('<div class="empty-state">Enter a search query</div>')

    agent_id = agent if agent else None
    results = await runtime.memory_manager.search(q, agent_id=agent_id, limit=20)

    if not results:
        return HTMLResponse(f'<div class="empty-state">No results for "{q}"</div>')

    html_parts = ['<div class="memory-list">']
    for result in results:
        memory = result.memory
        score = result.relevance_score
        tags_html = ""
        if memory.tags:
            tags_html = '<div class="tags">' + "".join(
                f'<span class="tag">#{t}</span>' for t in memory.tags[:3]
            ) + "</div>"

        # Escape content for HTML - handle None values safely
        content_text = memory.content if memory.content else "(no content)"
        summary_text = memory.summary if memory.summary else content_text[:100]
        safe_summary = html_module.escape(summary_text)
        safe_content = html_module.escape(content_text)
        truncated = len(summary_text) > 80
        display_summary = safe_summary[:80] + ('...' if truncated else '')

        html_parts.append(f"""
            <div class="memory-item" data-memory-id="{memory.id}">
                <div class="memory-header" onclick="toggleMemory(this)">
                    <div class="summary">{display_summary}</div>
                    <div class="memory-actions">
                        <span class="score">Score: {score:.2f}</span>
                        <button class="btn-outline-danger"
                                onclick="event.stopPropagation(); deleteMemory('{memory.id}')"
                                title="Delete memory">×</button>
                    </div>
                </div>
                <div class="memory-details" style="display: none;">
                    <div class="memory-content">{safe_content}</div>
                    <div class="meta">
                        <span class="layer">{memory.layer}</span>
                        <span class="date">{memory.created_at.strftime('%Y-%m-%d %H:%M')}</span>
                        <span class="id">ID: {memory.id[:8]}...</span>
                    </div>
                    {tags_html}
                </div>
            </div>
        """)

    html_parts.append("</div>")
    return HTMLResponse("".join(html_parts))
