"""Memory hooks for before/after message processing.

Implements MAPO-social style memory management:
- Short-term memory (8 slots) with spill to mid-term (vector store)
- Memory rewriting: useful memories are rewritten to be more useful next time
- New memories created from each conversation
"""

import logging
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from xpressai.memory.manager import MemoryManager
    from xpressai.core.config import MemoryConfig

logger = logging.getLogger(__name__)

# Track which memories were used in the current context (per-agent)
_used_memories: dict[str, list[dict]] = {}  # agent_id -> list of {memory, score}


def get_used_memories(agent_id: str) -> list[dict]:
    """Get memories that were used/retrieved for this agent."""
    return _used_memories.get(agent_id, [])


def clear_used_memories(agent_id: str) -> None:
    """Clear the used memories tracking for an agent."""
    _used_memories[agent_id] = []


def track_used_memory(agent_id: str, memory, score: float) -> None:
    """Track that a memory was used/retrieved."""
    if agent_id not in _used_memories:
        _used_memories[agent_id] = []
    _used_memories[agent_id].append({"memory": memory, "score": score})


async def memory_recall(
    agent_id: str,
    message: str,
    memory_manager: "MemoryManager",
    memory_config: "MemoryConfig",
    llm_callback=None,
) -> dict:
    """Recall relevant memories before processing a message.

    MAPO-social style:
    1. Get memories from short-term slots
    2. Search mid-term (vector) for relevant memories
    3. Evaluate which memories are useful for current context
    4. REMOVE useful memories from slots (they're "in use")
    5. Track them for potential rewrite after conversation

    Args:
        agent_id: ID of the agent
        message: The incoming message
        memory_manager: Memory manager instance
        memory_config: Memory configuration with prompts
        llm_callback: Optional async callback to call LLM

    Returns:
        Dict with:
            - context: Formatted memory context to inject
            - debug: Debug info for logging
    """
    debug_info = {
        "short_term_count": 0,
        "mid_term_count": 0,
        "useful_count": 0,
        "memories": [],
    }

    # Clear previous used memories tracking
    clear_used_memories(agent_id)

    try:
        contexts = []
        slot_manager = memory_manager.get_slot_manager(agent_id)

        # 1. Get short-term memories from slots
        slots = await memory_manager.get_slots(agent_id)
        short_term_memories = []
        for slot in slots:
            if slot.memory:
                short_term_memories.append({"memory": slot.memory, "slot_index": slot.index})

        debug_info["short_term_count"] = len(short_term_memories)

        # 2. Search mid-term (vector) for relevant memories
        mid_term_results = await memory_manager.search(
            message,
            limit=memory_config.near_term_slots,
            agent_id=agent_id,
        )
        debug_info["mid_term_count"] = len(mid_term_results)

        # 3. Evaluate usefulness and build context
        useful_memories = []

        # Evaluate short-term memories
        for item in short_term_memories:
            memory = item["memory"]
            is_useful = True  # Short-term memories are assumed useful by default

            if llm_callback:
                # Ask LLM if this memory is useful for current context
                eval_prompt = f"""Is this memory relevant to the current message?

MEMORY: {memory.summary}: {memory.content}

CURRENT MESSAGE: {message}

Reply with just "useful" or "useless"."""
                eval_result = await llm_callback(eval_prompt)
                is_useful = "useful" in eval_result.lower()

            if is_useful:
                useful_memories.append({"memory": memory, "score": 1.0, "source": "short_term"})
                track_used_memory(agent_id, memory, 1.0)

                # UNLOAD from slot - it's now "in use"
                await slot_manager.unload(item["slot_index"])
                logger.debug(f"Unloaded memory from slot {item['slot_index']} (in use)")

        # Add mid-term results (already filtered by relevance)
        for search_result in mid_term_results:
            # Don't duplicate if already in short-term
            if not any(m["memory"].id == search_result.memory.id for m in useful_memories):
                useful_memories.append({
                    "memory": search_result.memory,
                    "score": search_result.relevance_score,
                    "source": "mid_term"
                })
                track_used_memory(agent_id, search_result.memory, search_result.relevance_score)

        debug_info["useful_count"] = len(useful_memories)

        # 4. Format context for injection
        if useful_memories:
            # Short-term memories (full content)
            short_term = [m for m in useful_memories if m["source"] == "short_term"]
            if short_term:
                contexts.append("Recent memories:")
                for m in short_term:
                    mem = m["memory"]
                    contexts.append(f"- {mem.summary}: {mem.content}")
                    debug_info["memories"].append({
                        "summary": mem.summary,
                        "score": m["score"],
                        "source": "short_term"
                    })

            # Mid-term memories (just summaries)
            mid_term = [m for m in useful_memories if m["source"] == "mid_term"]
            if mid_term:
                contexts.append("\nRelevant past memories:")
                for m in mid_term:
                    mem = m["memory"]
                    contexts.append(f"- {mem.summary}")
                    debug_info["memories"].append({
                        "summary": mem.summary,
                        "score": m["score"],
                        "source": "mid_term"
                    })

        context_str = "\n".join(contexts) if contexts else ""
        logger.info(f"Memory recall: {len(useful_memories)} useful memories for agent {agent_id}")
        return {"context": context_str, "debug": debug_info}

    except Exception as e:
        logger.error(f"Memory recall failed: {e}")
        debug_info["error"] = str(e)
        return {"context": "", "debug": debug_info}


async def memory_remember(
    agent_id: str,
    conversation: list[dict],
    memory_manager: "MemoryManager",
    memory_config: "MemoryConfig",
    llm_callback=None,
) -> dict | bool:
    """Process memories after conversation - MAPO-social style.

    1. Evaluate if used memories were actually useful
    2. Rewrite useful memories to be even more useful next time
    3. Create a new memory from the conversation
    4. Handle slot overflow (spill to mid-term/vector)

    Args:
        agent_id: ID of the agent
        conversation: List of message dicts
        memory_manager: Memory manager instance
        memory_config: Memory configuration
        llm_callback: Optional LLM callback

    Returns:
        dict with 'stored' bool and 'debug' info, or False on error
    """
    result = {"stored": False, "debug": {}}

    try:
        if not llm_callback:
            logger.debug("No LLM callback for memory_remember, skipping")
            result["debug"]["error"] = "No LLM callback"
            return result

        # Format conversation
        conv_text = []
        for msg in conversation[-10:]:
            if isinstance(msg, dict):
                role = msg.get("role", "unknown").upper()
                content = msg.get("content", "")
            else:
                logger.warning(f"Unexpected message type: {type(msg)}")
                role = "UNKNOWN"
                content = str(msg)
            conv_text.append(f"[{role}]: {content}")
        conversation_str = "\n".join(conv_text)
        result["debug"]["conversation"] = conversation_str[:200]

        # 1. Get memories that were used in this conversation
        used_memories = get_used_memories(agent_id)

        # 2. Evaluate and rewrite useful memories
        for used in used_memories:
            memory = used["memory"]

            # Ask if memory was actually useful
            was_useful_prompt = f"""Was this memory useful for the conversation?

MEMORY: {memory.summary}: {memory.content}

CONVERSATION:
{conversation_str}

Reply with just "useful" or "useless"."""

            useful_response = await llm_callback(was_useful_prompt)
            was_useful = "useful" in useful_response.lower()

            if was_useful:
                # Rewrite the memory to be more useful next time
                rewrite_prompt = f"""Update this memory with any new information from the conversation.
Preserve all existing facts and add any new relevant details.

ORIGINAL MEMORY:
Summary: {memory.summary}
Content: {memory.content}

CONVERSATION WHERE IT WAS USED:
{conversation_str}

Rules:
- Keep all original facts unless they're now outdated
- Add new facts learned from the conversation
- Make it more specific and detailed, not less
- Use bullet points for clarity

Format your response as:
SUMMARY: <updated one-line summary>
CONTENT: <updated content with all facts>"""

                rewrite_result = await llm_callback(rewrite_prompt)

                # Parse rewritten memory
                new_summary = memory.summary
                new_content = memory.content
                for line in rewrite_result.split("\n"):
                    line_upper = line.upper()
                    if line_upper.startswith("SUMMARY:"):
                        new_summary = line[8:].strip()
                    elif line_upper.startswith("CONTENT:"):
                        new_content = line[8:].strip()

                # Update the memory
                memory.summary = new_summary
                memory.content = new_content
                await memory_manager.update(memory)
                logger.info(f"Memory rewritten for agent {agent_id}: {new_summary}")

        # 3. Create a new memory from this conversation
        create_prompt = f"""You are a memory extraction system. Your job is to extract and save important facts.

CONVERSATION:
{conversation_str}

TASK: Extract facts worth remembering from this conversation.

Examples of what to extract:
- Company names, what they do, their products
- People's names, roles, preferences
- Technical details, URLs, file paths
- Decisions, agreements, conclusions
- Any specific information that would be useful later

IMPORTANT: If the conversation contains ANY factual information (company info, product details, names, URLs, technical details, etc.), you MUST extract it.

Only respond with exactly "NOTHING" if the conversation is literally just greetings like "hi" or "thanks" with zero information.

Format your response as:
SUMMARY: <one line describing what this is about>
CONTENT: <bullet points of facts>
TAGS: <keywords>"""

        create_result = await llm_callback(create_prompt)

        # Ensure we have a string response
        if not isinstance(create_result, str):
            logger.warning(f"LLM callback returned non-string: {type(create_result)}")
            create_result = str(create_result)

        logger.info(f"Memory create LLM response: {create_result[:300]}...")
        result["debug"]["llm_response"] = create_result[:500]

        # Check if LLM said nothing to remember
        result_upper = create_result.upper().strip()
        has_content = not (result_upper == "NOTHING" or result_upper.startswith("NOTHING.") or result_upper.startswith("NOTHING\n") or result_upper.startswith("NOTHING "))

        if has_content:
            # Parse new memory
            summary = ""
            content = ""
            tags = []

            # Handle multi-line content
            lines = create_result.split("\n")
            current_field = None
            content_lines = []

            for line in lines:
                line_upper = line.upper().strip()
                if line_upper.startswith("SUMMARY:"):
                    summary = line[8:].strip()
                    current_field = "summary"
                elif line_upper.startswith("CONTENT:"):
                    content = line[8:].strip()
                    current_field = "content"
                elif line_upper.startswith("TAGS:"):
                    tags_str = line[5:].strip()
                    tags = [t.strip() for t in tags_str.split(",") if t.strip()]
                    current_field = "tags"
                elif current_field == "content" and line.strip():
                    # Append to content (for multi-line bullet points)
                    content_lines.append(line.strip())

            # Join content lines if we captured multi-line content
            if content_lines:
                content = content + "\n" + "\n".join(content_lines) if content else "\n".join(content_lines)

            result["debug"]["parsed"] = {"summary": summary, "content": content[:200], "tags": tags}

            if summary and content:
                # 4. Add to short-term slots (handles overflow to mid-term automatically)
                new_memory = await memory_manager.add(
                    content=content,
                    summary=summary,
                    tags=tags,
                    source="agent_remember",
                    layer="agent",
                    agent_id=agent_id,
                )

                # Load into slot (will spill oldest to vector if full)
                await memory_manager.load_to_slot(agent_id, new_memory, relevance_score=1.0)

                logger.info(f"New memory created for agent {agent_id}: {summary}")
                result["stored"] = True
                result["debug"]["memory_id"] = new_memory.id
                return result
            else:
                result["debug"]["parse_error"] = f"Missing summary ({bool(summary)}) or content ({bool(content)})"
        else:
            result["debug"]["skipped"] = "LLM returned NOTHING"

        # Clear used memories tracking
        clear_used_memories(agent_id)
        return result

    except Exception as e:
        import traceback
        tb = traceback.format_exc()
        logger.error(f"Memory remember failed: {e}\n{tb}")
        result["debug"]["error"] = str(e)
        result["debug"]["traceback"] = tb
        return result


# Hook registry
HOOKS = {
    "memory_recall": memory_recall,
    "memory_remember": memory_remember,
}


async def run_before_message_hooks(
    hooks: list[str],
    agent_id: str,
    message: str,
    memory_manager: "MemoryManager",
    memory_config: "MemoryConfig",
    llm_callback=None,
) -> str:
    """Run all before_message hooks and return combined context.

    Args:
        hooks: List of hook names to run
        agent_id: ID of the agent
        message: The incoming message
        memory_manager: Memory manager instance
        memory_config: Memory configuration
        llm_callback: Optional LLM callback

    Returns:
        Combined context from all hooks
    """
    contexts = []

    for hook_name in hooks:
        if hook_name == "memory_recall":
            result = await memory_recall(
                agent_id, message, memory_manager, memory_config, llm_callback
            )
            ctx = result.get("context", "") if isinstance(result, dict) else result
            if ctx:
                contexts.append(ctx)
        else:
            logger.warning(f"Unknown before_message hook: {hook_name}")

    return "\n\n".join(contexts)


async def run_after_message_hooks(
    hooks: list[str],
    agent_id: str,
    conversation: list[dict],
    memory_manager: "MemoryManager",
    memory_config: "MemoryConfig",
    llm_callback=None,
) -> None:
    """Run all after_message hooks.

    Args:
        hooks: List of hook names to run
        agent_id: ID of the agent
        conversation: Recent conversation messages
        memory_manager: Memory manager instance
        memory_config: Memory configuration
        llm_callback: Optional LLM callback
    """
    for hook_name in hooks:
        if hook_name == "memory_remember":
            await memory_remember(
                agent_id, conversation, memory_manager, memory_config, llm_callback
            )
        else:
            logger.warning(f"Unknown after_message hook: {hook_name}")
