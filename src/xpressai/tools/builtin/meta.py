"""Meta tools for agent self-management.

These tools allow agents to create tasks, memories, and procedures
during direct conversations (not task-based execution).
"""

from typing import Any
import logging

logger = logging.getLogger(__name__)

# These will be set by the chat handler
_task_board = None
_memory_manager = None
_sop_manager = None
_current_agent_id = None
_in_task_context = False  # True when agent is executing a task (not chat)
_current_task_id = None


def set_managers(
    task_board,
    memory_manager,
    sop_manager,
    agent_id: str | None = None,
    in_task_context: bool = False,
    task_id: str | None = None,
):
    """Set the manager instances for meta tools.

    Args:
        task_board: TaskBoard instance
        memory_manager: MemoryManager instance
        sop_manager: SOPManager instance
        agent_id: Current agent ID (for default task assignment)
        in_task_context: True if agent is executing a task (prevents new task creation)
        task_id: Current task ID if in task context
    """
    global _task_board, _memory_manager, _sop_manager, _current_agent_id
    global _in_task_context, _current_task_id
    _task_board = task_board
    _memory_manager = memory_manager
    _sop_manager = sop_manager
    _current_agent_id = agent_id
    _in_task_context = in_task_context
    _current_task_id = task_id


async def create_task(
    title: str,
    description: str | None = None,
    agent_id: str | None = None,
    priority: int = 0,
) -> dict[str, Any]:
    """Create a new task on the task board.

    Args:
        title: Task title (required)
        description: Optional task description
        agent_id: Optional agent to assign the task to (defaults to current agent)
        priority: Task priority (higher = more important)

    Returns:
        Created task info
    """
    # Block task creation when already executing a task
    if _in_task_context:
        return {
            "error": "Cannot create new tasks while executing a task. "
            "You are already working on the assigned task - complete the work directly "
            "instead of creating new tasks. If you need to break down the work, "
            "just proceed step by step within this task.",
            "hint": "Focus on completing the current task using the available tools.",
        }

    if not _task_board:
        return {"error": "Task board not available"}

    # Default to current agent if not specified
    assigned_agent = agent_id if agent_id is not None else _current_agent_id

    task = await _task_board.create_task(
        title=title,
        description=description,
        agent_id=assigned_agent,
        priority=priority,
    )

    logger.info(f"Meta tool created task: {task.id} - {title}")

    return {
        "success": True,
        "task_id": task.id,
        "title": task.title,
        "status": task.status.value,
        "agent_id": task.agent_id,
    }


async def create_memory(
    content: str,
    summary: str,
    tags: list[str] | None = None,
) -> dict[str, Any]:
    """Create a new memory/note in the knowledge base.

    Args:
        content: Full content of the memory
        summary: Brief summary (1-2 sentences)
        tags: Optional list of tags for categorization

    Returns:
        Created memory info
    """
    if not _memory_manager:
        return {"error": "Memory manager not available"}

    memory = await _memory_manager.add(
        content=content,
        summary=summary,
        source="agent_chat",
        tags=tags or [],
        layer="agent",
        agent_id=_current_agent_id,
    )

    logger.info(f"Meta tool created memory: {memory.id} - {summary[:50]}")

    return {
        "success": True,
        "memory_id": memory.id,
        "summary": memory.summary,
        "tags": list(memory.tags),
    }


async def search_memory(
    query: str,
    limit: int = 5,
) -> dict[str, Any]:
    """Search the knowledge base for relevant memories.

    Args:
        query: Search query - what you're looking for
        limit: Maximum number of results (default 5)

    Returns:
        List of matching memories
    """
    if not _memory_manager:
        return {"error": "Memory manager not available"}

    results = await _memory_manager.search(
        query=query,
        limit=limit,
        agent_id=_current_agent_id,
    )

    memories = []
    for result in results:
        memories.append({
            "id": result.memory.id,
            "summary": result.memory.summary,
            "content": result.memory.content,
            "tags": list(result.memory.tags) if result.memory.tags else [],
            "relevance": round(result.relevance_score, 2),
        })

    logger.info(f"Meta tool searched memory: '{query}' -> {len(memories)} results")

    return {
        "success": True,
        "query": query,
        "count": len(memories),
        "memories": memories,
    }


async def create_procedure(
    name: str,
    description: str,
    steps: list[str],
    triggers: list[str] | None = None,
) -> dict[str, Any]:
    """Create a new Standard Operating Procedure (SOP).

    Args:
        name: Procedure name (unique identifier)
        description: What this procedure does
        steps: List of step descriptions in order
        triggers: Optional list of trigger phrases that activate this SOP

    Returns:
        Created SOP info
    """
    if not _sop_manager:
        return {"error": "SOP manager not available"}

    try:
        sop = _sop_manager.create(
            name=name,
            description=description,
            steps=[{"prompt": step} for step in steps],
            triggers=triggers or [],
        )

        logger.info(f"Meta tool created SOP: {sop.id} - {name}")

        return {
            "success": True,
            "sop_id": sop.id,
            "name": sop.name,
            "steps_count": len(steps),
        }
    except Exception as e:
        return {"error": str(e)}


async def list_agents() -> dict[str, Any]:
    """List all available agents.

    Returns:
        List of agent names and their status
    """
    if not _task_board:
        return {"error": "System not available"}

    # This would need runtime access - for now return a placeholder
    return {
        "message": "Use the dashboard to see available agents",
    }


def get_meta_tool_schemas() -> list[dict[str, Any]]:
    """Get tool schemas for meta tools.

    Returns:
        List of tool definitions in OpenAI function format
    """
    return [
        {
            "name": "create_task",
            "description": "Create a new task on the task board. Use this when asked to do something that requires tools you don't have access to in this chat (like file operations, running commands, or complex multi-step work). Tasks are processed by agents with full tool access. Also use this to schedule work for other agents.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Task title - what needs to be done",
                    },
                    "description": {
                        "type": "string",
                        "description": "Detailed description of the task",
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Agent to assign the task to. Use your own agent name to assign to yourself, or leave empty for unassigned.",
                    },
                    "priority": {
                        "type": "integer",
                        "description": "Priority level (higher = more important). Default is 0.",
                        "default": 0,
                    },
                },
                "required": ["title"],
            },
        },
        {
            "name": "create_memory",
            "description": "CRITICAL: You have ANTEROGRADE AMNESIA - you cannot form new long-term memories naturally. Use this tool to save any important information you learn - company details, user preferences, technical decisions, contacts, URLs, etc. If you don't save it, you WILL forget it after this conversation ends.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "Full content of the memory - be detailed, include all relevant facts",
                    },
                    "summary": {
                        "type": "string",
                        "description": "Brief 1-2 sentence summary for quick reference",
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Tags for categorization (e.g., ['company', 'contact', 'technical', 'preference'])",
                    },
                },
                "required": ["content", "summary"],
            },
        },
        {
            "name": "search_memory",
            "description": "Search your knowledge base for information you've previously saved. Use this at the start of conversations to recall relevant context, or whenever you need to look up something you might have learned before.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What you're looking for - can be a topic, name, keyword, or question",
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default 5)",
                        "default": 5,
                    },
                },
                "required": ["query"],
            },
        },
        {
            "name": "create_procedure",
            "description": "Create a Standard Operating Procedure (SOP) - a reusable workflow that agents can follow for consistent task execution.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Unique name for the procedure (e.g., 'deploy-to-production')",
                    },
                    "description": {
                        "type": "string",
                        "description": "What this procedure accomplishes",
                    },
                    "steps": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "List of steps in order, each describing what to do",
                    },
                    "triggers": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Phrases that should trigger this procedure (e.g., ['deploy', 'ship it'])",
                    },
                },
                "required": ["name", "description", "steps"],
            },
        },
    ]


# Tool handlers map
META_TOOLS = {
    "create_task": create_task,
    "create_memory": create_memory,
    "search_memory": search_memory,
    "create_procedure": create_procedure,
}


async def execute_meta_tool(tool_name: str, arguments: dict[str, Any]) -> str:
    """Execute a meta tool and return the result as a string.

    Args:
        tool_name: Name of the tool to execute
        arguments: Tool arguments

    Returns:
        Result as JSON string
    """
    import json

    handler = META_TOOLS.get(tool_name)
    if not handler:
        return json.dumps({"error": f"Unknown tool: {tool_name}"})

    try:
        result = await handler(**arguments)
        return json.dumps(result, indent=2)
    except Exception as e:
        return json.dumps({"error": str(e)})
