"""MCP stdio server for xpressclaw memory system.

Provides memory tools (search, save, list) over the MCP protocol.
Calls back to the xpressclaw server REST API.

Environment variables:
  XPRESSCLAW_URL  — Base URL of the xpressclaw server
  AGENT_ID        — Current agent's ID
"""

import json
import os
import sys

import httpx

BASE_URL = os.environ.get(
    "XPRESSCLAW_URL",
    f"http://host.docker.internal:{os.environ.get('XPRESSCLAW_PORT', '8935')}",
)
AGENT_ID = os.environ.get("AGENT_ID", "")

TOOLS = [
    {
        "name": "search_memory",
        "description": (
            "Search your memory for relevant information. Use before starting "
            "any task to recall context about the user, project, or topic."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query (semantic search over all memories)",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 10)",
                    "default": 10,
                },
            },
            "required": ["query"],
        },
    },
    {
        "name": "save_memory",
        "description": (
            "Save important information to long-term memory. Use when you learn "
            "something worth remembering: user preferences, project details, "
            "decisions made, facts about people or systems."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "The information to remember",
                },
                "summary": {
                    "type": "string",
                    "description": "Brief one-line summary for quick recall",
                },
                "tags": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Tags for categorization (e.g. 'user-pref', 'project', 'decision')",
                },
            },
            "required": ["content", "summary"],
        },
    },
    {
        "name": "list_memories",
        "description": "List recent memories, optionally filtered by tag.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tag": {
                    "type": "string",
                    "description": "Filter by tag (optional)",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results (default 20)",
                    "default": 20,
                },
            },
        },
    },
    {
        "name": "delete_memory",
        "description": "Delete a memory by ID.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Memory ID to delete",
                },
            },
            "required": ["id"],
        },
    },
]


def _api(method: str, path: str, body: dict | None = None, params: dict | None = None) -> dict | list:
    url = f"{BASE_URL}/api/memory{path}"
    with httpx.Client(timeout=15) as client:
        if method == "GET":
            r = client.get(url, params=params)
        elif method == "POST":
            r = client.post(url, json=body)
        elif method == "DELETE":
            r = client.delete(url)
        else:
            raise ValueError(f"unsupported method: {method}")
        r.raise_for_status()
        return r.json()


def handle_tool(name: str, arguments: dict) -> str:
    if name == "search_memory":
        query = arguments["query"]
        limit = arguments.get("limit", 10)
        results = _api("GET", "/search", params={"q": query, "limit": limit})
        if not results:
            return f"No memories found for: {query}"
        lines = []
        for m in results:
            lines.append(f"[{m['id'][:8]}] {m.get('summary', '')}")
            lines.append(f"  {m.get('content', '')[:200]}")
            if m.get('tags'):
                lines.append(f"  tags: {', '.join(m['tags'])}")
            lines.append("")
        return "\n".join(lines)

    elif name == "save_memory":
        body = {
            "content": arguments["content"],
            "summary": arguments.get("summary", arguments["content"][:100]),
            "source": f"agent:{AGENT_ID}",
            "layer": "agent",
            "agent_id": AGENT_ID,
            "tags": arguments.get("tags", []),
        }
        result = _api("POST", "", body=body)
        return f"Saved memory: {result.get('id', 'ok')}"

    elif name == "list_memories":
        params = {"limit": arguments.get("limit", 20)}
        if "tag" in arguments:
            params["tag"] = arguments["tag"]
        if AGENT_ID:
            params["agent_id"] = AGENT_ID
        results = _api("GET", "", params=params)
        if not results:
            return "No memories stored yet."
        lines = []
        for m in results:
            tags = f" [{', '.join(m.get('tags', []))}]" if m.get('tags') else ""
            lines.append(f"- [{m['id'][:8]}] {m.get('summary', '')}{tags}")
        return "\n".join(lines)

    elif name == "delete_memory":
        _api("DELETE", f"/{arguments['id']}")
        return f"Deleted memory {arguments['id']}"

    raise ValueError(f"unknown tool: {name}")


# --- MCP stdio protocol ---

def _read_message():
    header = ""
    while True:
        line = sys.stdin.readline()
        if not line:
            return None
        header += line
        if header.endswith("\r\n\r\n") or header.endswith("\n\n"):
            break
    length = 0
    for h in header.strip().split("\n"):
        if h.lower().startswith("content-length:"):
            length = int(h.split(":", 1)[1].strip())
    if length == 0:
        return None
    body = sys.stdin.read(length)
    return json.loads(body)


def _write_message(obj: dict):
    body = json.dumps(obj)
    header = f"Content-Length: {len(body)}\r\n\r\n"
    sys.stdout.write(header + body)
    sys.stdout.flush()


def _response(msg_id, result):
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def _error_response(msg_id, code, message):
    return {"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}}


def main():
    while True:
        msg = _read_message()
        if msg is None:
            break

        msg_id = msg.get("id")
        method = msg.get("method", "")
        params = msg.get("params", {})

        if method == "initialize":
            _write_message(
                _response(
                    msg_id,
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "xpressclaw-memory",
                            "version": "0.1.0",
                        },
                    },
                )
            )
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            _write_message(_response(msg_id, {"tools": TOOLS}))
        elif method == "tools/call":
            tool_name = params.get("name", "")
            arguments = params.get("arguments", {})
            try:
                result_text = handle_tool(tool_name, arguments)
                _write_message(
                    _response(
                        msg_id,
                        {
                            "content": [{"type": "text", "text": result_text}],
                            "isError": False,
                        },
                    )
                )
            except Exception as e:
                _write_message(
                    _response(
                        msg_id,
                        {
                            "content": [{"type": "text", "text": f"Error: {e}"}],
                            "isError": True,
                        },
                    )
                )
        elif method == "notifications/cancelled":
            pass
        else:
            if msg_id is not None:
                _write_message(
                    _error_response(msg_id, -32601, f"method not found: {method}")
                )


if __name__ == "__main__":
    main()
