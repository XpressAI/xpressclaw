"""XpressAI memory command - Inspect agent memory state."""

import asyncio
from pathlib import Path
import click

from xpressai.core.config import load_config
from xpressai.core.runtime import Runtime
from xpressai.core.exceptions import DatabaseError


def _handle_db_error(e: Exception) -> None:
    """Handle database errors with user-friendly message."""
    error_str = str(e).lower()
    if "no such table" in error_str:
        click.echo(click.style("No memories found. The memory system hasn't been used yet.", fg="yellow"))
        click.echo("Run 'xpressai up' to start the runtime and create the database.")
    else:
        click.echo(click.style(f"Database error: {e}", fg="red"))


def run_memory_list(agent: str | None = None, limit: int = 10) -> None:
    """List recent memories."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        return

    config = load_config(config_path)
    try:
        asyncio.run(_list_memories(config, agent, limit))
    except (DatabaseError, Exception) as e:
        _handle_db_error(e)


async def _list_memories(config, agent: str | None, limit: int) -> None:
    """List recent memories."""
    runtime = Runtime(config)
    await runtime.initialize()

    memories = await runtime.memory_manager.get_recent(agent_id=agent, limit=limit)

    if not memories:
        click.echo(click.style("No memories found.", fg="yellow"))
        return

    click.echo(click.style("Recent Memories:", fg="cyan", bold=True))
    click.echo()

    for memory in memories:
        _display_memory_summary(memory)


def run_memory_search(query: str, agent: str | None = None, limit: int = 10) -> None:
    """Search memories."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        return

    config = load_config(config_path)
    try:
        asyncio.run(_search_memories(config, query, agent, limit))
    except (DatabaseError, Exception) as e:
        _handle_db_error(e)


async def _search_memories(config, query: str, agent: str | None, limit: int) -> None:
    """Search memories by query."""
    runtime = Runtime(config)
    await runtime.initialize()

    results = await runtime.memory_manager.search(query, agent_id=agent, limit=limit)

    if not results:
        click.echo(click.style(f"No memories found for '{query}'", fg="yellow"))
        return

    click.echo(click.style(f"Search Results for '{query}':", fg="cyan", bold=True))
    click.echo()

    for result in results:
        _display_memory_summary(result.memory, relevance=result.relevance_score)


def run_memory_show(memory_id: str) -> None:
    """Show a specific memory."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        return

    config = load_config(config_path)
    try:
        asyncio.run(_show_memory(config, memory_id))
    except (DatabaseError, Exception) as e:
        _handle_db_error(e)


async def _show_memory(config, memory_id: str) -> None:
    """Show a specific memory in detail."""
    runtime = Runtime(config)
    await runtime.initialize()

    try:
        memory = await runtime.memory_manager.get(memory_id)
    except Exception as e:
        click.echo(click.style(f"Memory not found: {memory_id}", fg="red"))
        return

    click.echo(click.style(f"Memory: {memory.id}", fg="cyan", bold=True))
    click.echo()

    click.echo(f"  Summary: {memory.summary}")
    click.echo(f"  Layer:   {memory.layer}")
    click.echo(f"  Source:  {memory.source}")
    if memory.agent_id:
        click.echo(f"  Agent:   {memory.agent_id}")
    if memory.tags:
        click.echo(f"  Tags:    {', '.join(memory.tags)}")
    click.echo(f"  Created: {memory.created_at.strftime('%Y-%m-%d %H:%M')}")
    click.echo(f"  Accessed: {memory.accessed_at.strftime('%Y-%m-%d %H:%M')} ({memory.access_count}x)")

    if memory.links:
        click.echo(f"  Links:   {len(memory.links)}")
    if memory.backlinks:
        click.echo(f"  Backlinks: {len(memory.backlinks)}")

    click.echo()
    click.echo(click.style("Content:", bold=True))
    click.echo()
    # Indent content
    for line in memory.content.split("\n"):
        click.echo(f"  {line}")


def run_memory_stats() -> None:
    """Show memory system statistics."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        return

    config = load_config(config_path)
    try:
        asyncio.run(_show_stats(config))
    except (DatabaseError, Exception) as e:
        _handle_db_error(e)


async def _show_stats(config) -> None:
    """Show memory system statistics."""
    runtime = Runtime(config)
    await runtime.initialize()

    stats = await runtime.memory_manager.get_stats()

    click.echo(click.style("Memory System Stats:", fg="cyan", bold=True))
    click.echo()

    zettel = stats.get("zettelkasten", {})
    click.echo(click.style("Zettelkasten:", bold=True))
    click.echo(f"  Total memories: {zettel.get('total_memories', 0)}")
    click.echo(f"  Total links:    {zettel.get('total_links', 0)}")

    by_layer = zettel.get("by_layer", {})
    if by_layer:
        click.echo("  By layer:")
        for layer, count in by_layer.items():
            click.echo(f"    {layer}: {count}")

    click.echo()

    vector = stats.get("vector_store", {})
    click.echo(click.style("Vector Store:", bold=True))
    click.echo(f"  Embeddings: {vector.get('total_embeddings', 0)}")

    click.echo()

    conf = stats.get("config", {})
    click.echo(click.style("Configuration:", bold=True))
    click.echo(f"  Near-term slots: {conf.get('near_term_slots', 8)}")
    click.echo(f"  Eviction policy: {conf.get('eviction', 'least-recently-relevant')}")


def run_memory_slots(agent: str) -> None:
    """Show memory slots for an agent."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        return

    config = load_config(config_path)
    try:
        asyncio.run(_show_slots(config, agent))
    except (DatabaseError, Exception) as e:
        _handle_db_error(e)


async def _show_slots(config, agent: str) -> None:
    """Show memory slots for an agent."""
    runtime = Runtime(config)
    await runtime.initialize()

    slots = await runtime.memory_manager.get_slots(agent)

    click.echo(click.style(f"Memory Slots for {agent}:", fg="cyan", bold=True))
    click.echo()

    if not any(s.memory_id for s in slots):
        click.echo(click.style("  (all slots empty)", fg="yellow"))
        return

    for i, slot in enumerate(slots):
        if slot.memory_id:
            try:
                memory = await runtime.memory_manager.get(slot.memory_id)
                click.echo(f"  [{i}] {memory.summary[:60]}...")
                click.echo(f"      Relevance: {slot.relevance_score:.2f}")
            except Exception:
                click.echo(f"  [{i}] {slot.memory_id} (not found)")
        else:
            click.echo(click.style(f"  [{i}] (empty)", fg="white"))


def run_memory_delete(memory_id: str) -> None:
    """Delete a memory."""
    config_path = Path.cwd() / "xpressai.yaml"

    if not config_path.exists():
        click.echo(click.style("No xpressai.yaml found.", fg="yellow"))
        return

    config = load_config(config_path)
    try:
        asyncio.run(_delete_memory(config, memory_id))
    except (DatabaseError, Exception) as e:
        _handle_db_error(e)


async def _delete_memory(config, memory_id: str) -> None:
    """Delete a memory by ID."""
    runtime = Runtime(config)
    await runtime.initialize()

    # Find memory by ID prefix
    all_memories = await runtime.memory_manager.get_recent(limit=1000)
    matching = [m for m in all_memories if m.id.startswith(memory_id)]

    if not matching:
        click.echo(click.style(f"Memory not found: {memory_id}", fg="red"))
        return

    if len(matching) > 1:
        click.echo(click.style(f"Multiple memories match '{memory_id}'. Be more specific.", fg="red"))
        for m in matching:
            click.echo(f"  {m.id[:8]}... - {m.summary[:40]}...")
        return

    memory = matching[0]

    # Confirm deletion
    click.echo(f"Memory: {memory.summary}")
    if not click.confirm("Delete this memory?"):
        click.echo("Cancelled.")
        return

    await runtime.memory_manager.delete(memory.id)
    click.echo(click.style(f"Deleted memory: {memory.id[:8]}...", fg="green"))


def _display_memory_summary(memory, relevance: float | None = None) -> None:
    """Display a memory in summary format."""
    layer_colors = {
        "shared": "blue",
        "agent": "green",
        "user": "magenta",
    }
    color = layer_colors.get(memory.layer, "white")

    # ID (shortened)
    short_id = memory.id[:8]

    # Build line
    line = f"  {click.style(short_id, fg='cyan')} "
    line += f"[{click.style(memory.layer, fg=color)}] "
    line += memory.summary[:50]
    if len(memory.summary) > 50:
        line += "..."

    if relevance is not None:
        line += f" ({relevance:.2f})"

    click.echo(line)

    # Tags
    if memory.tags:
        tags_str = " ".join(f"#{t}" for t in memory.tags[:5])
        click.echo(f"      {click.style(tags_str, fg='yellow')}")
