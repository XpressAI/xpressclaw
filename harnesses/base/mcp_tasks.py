"""MCP stdio server for xpressclaw task management.

Provides task tools (create, list, update, complete, subtasks) over the MCP
protocol via stdin/stdout. Calls back to the xpressclaw server REST API.

Environment variables:
  XPRESSCLAW_URL  — Base URL of the xpressclaw server (default: http://host.docker.internal:8935)
  AGENT_ID        — Current agent's ID (for default task assignment)
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
        "name": "create_task",
        "description": (
            "Create a new task on the task board. Can assign to yourself or "
            "another agent. Use parent_task_id to create subtasks."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "Task title"},
                "description": {
                    "type": "string",
                    "description": "Detailed task description",
                },
                "agent_id": {
                    "type": "string",
                    "description": "Agent to assign (omit to assign to yourself)",
                },
                "parent_task_id": {
                    "type": "string",
                    "description": "Parent task ID to create this as a subtask",
                },
                "conversation_id": {
                    "type": "string",
                    "description": "Conversation ID to link this task to (for status notifications)",
                },
                "priority": {
                    "type": "integer",
                    "description": "0=low, 1=normal, 2=high, 3=urgent",
                    "default": 1,
                },
            },
            "required": ["title"],
        },
    },
    {
        "name": "list_tasks",
        "description": "List tasks from the task board. Filter by status and/or agent.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": [
                        "pending",
                        "in_progress",
                        "waiting_for_input",
                        "blocked",
                        "completed",
                        "cancelled",
                    ],
                    "description": "Filter by status",
                },
                "agent_id": {
                    "type": "string",
                    "description": "Filter by assigned agent",
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
        "name": "get_task",
        "description": "Get full details of a specific task by ID.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": {"type": "string", "description": "Task ID"},
            },
            "required": ["task_id"],
        },
    },
    {
        "name": "update_task",
        "description": (
            "Update a task's title, description, or status. "
            "Statuses: pending, in_progress, waiting_for_input, blocked, completed, cancelled."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": {"type": "string", "description": "Task ID to update"},
                "title": {"type": "string", "description": "New title"},
                "description": {"type": "string", "description": "New description"},
                "status": {
                    "type": "string",
                    "enum": [
                        "pending",
                        "in_progress",
                        "waiting_for_input",
                        "blocked",
                        "completed",
                        "cancelled",
                    ],
                    "description": "New status",
                },
            },
            "required": ["task_id"],
        },
    },
    {
        "name": "complete_task",
        "description": "Mark a task as completed.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": {"type": "string", "description": "Task ID to complete"},
            },
            "required": ["task_id"],
        },
    },
    {
        "name": "list_subtasks",
        "description": "List subtasks of a parent task.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "parent_task_id": {
                    "type": "string",
                    "description": "Parent task ID",
                },
            },
            "required": ["parent_task_id"],
        },
    },
    {
        "name": "create_schedule",
        "description": (
            "Create a recurring scheduled task using a cron expression. "
            "The task will be automatically created and assigned to an agent "
            "each time the schedule fires. "
            "Cron format: minute hour day-of-month month day-of-week "
            "(e.g. '0 9 * * *' = daily at 9am, '0 9 * * 1-5' = weekdays at 9am)."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Schedule name (e.g. 'Daily standup report')",
                },
                "cron": {
                    "type": "string",
                    "description": "Cron expression (e.g. '0 9 * * *' for daily at 9am)",
                },
                "title": {
                    "type": "string",
                    "description": "Task title template. Use {date}, {time}, {datetime} for placeholders.",
                },
                "description": {
                    "type": "string",
                    "description": "Task description — what the agent should do each time",
                },
                "agent_id": {
                    "type": "string",
                    "description": "Agent to assign (omit to assign to yourself)",
                },
            },
            "required": ["name", "cron", "title"],
        },
    },
    {
        "name": "list_schedules",
        "description": "List all scheduled tasks.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "agent_id": {
                    "type": "string",
                    "description": "Filter by agent",
                },
            },
        },
    },
    {
        "name": "delete_schedule",
        "description": "Delete a scheduled task by ID.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "schedule_id": {"type": "string", "description": "Schedule ID to delete"},
            },
            "required": ["schedule_id"],
        },
    },
    {
        "name": "list_procedures",
        "description": "List all available standard operating procedures (SOPs).",
        "inputSchema": {
            "type": "object",
            "properties": {},
        },
    },
    {
        "name": "run_procedure",
        "description": (
            "Run a standard operating procedure (SOP) by name. "
            "Creates a task from the procedure and assigns it to an agent. "
            "The procedure's steps will be executed automatically."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Procedure name",
                },
                "agent_id": {
                    "type": "string",
                    "description": "Agent to run the procedure (omit for yourself)",
                },
                "inputs": {
                    "type": "object",
                    "description": "Input values for the procedure (key-value pairs)",
                },
            },
            "required": ["name"],
        },
    },
]


def _api(method: str, path: str, body: dict | None = None) -> dict:
    """Call the xpressclaw REST API."""
    url = f"{BASE_URL}/api{path}"
    with httpx.Client(timeout=30) as client:
        if method == "GET":
            r = client.get(url, params=body)
        elif method == "POST":
            r = client.post(url, json=body)
        elif method == "PATCH":
            r = client.patch(url, json=body)
        elif method == "DELETE":
            r = client.delete(url)
        else:
            raise ValueError(f"unknown method: {method}")
        r.raise_for_status()
        return r.json() if r.status_code != 204 else {}


def handle_tool(name: str, arguments: dict) -> str:
    """Execute a task tool and return a text result."""
    if name == "create_task":
        body = {"title": arguments["title"]}
        if "description" in arguments:
            body["description"] = arguments["description"]
        body["agent_id"] = arguments.get("agent_id", AGENT_ID or None)
        if "parent_task_id" in arguments:
            body["parent_task_id"] = arguments["parent_task_id"]
        if "conversation_id" in arguments:
            body["conversation_id"] = arguments["conversation_id"]
        if "priority" in arguments:
            body["priority"] = arguments["priority"]

        task = _api("POST", "/tasks", body)
        return (
            f"Created task '{task['title']}' "
            f"(id: {task['id']}, assigned to: {task.get('agent_id') or 'unassigned'})"
        )

    elif name == "list_tasks":
        params = {}
        if "status" in arguments:
            params["status"] = arguments["status"]
        if "agent_id" in arguments:
            params["agent_id"] = arguments["agent_id"]
        if "limit" in arguments:
            params["limit"] = arguments["limit"]

        resp = _api("GET", "/tasks", params)
        tasks = resp.get("tasks", [])
        if not tasks:
            return "No tasks found."
        lines = []
        for t in tasks:
            lines.append(
                f"- [{t['status']}] {t['title']} "
                f"(id: {t['id']}, agent: {t.get('agent_id') or 'unassigned'}, priority: {t.get('priority', 0)})"
            )
        return f"{len(tasks)} task(s):\n" + "\n".join(lines)

    elif name == "get_task":
        task = _api("GET", f"/tasks/{arguments['task_id']}")
        return json.dumps(task, indent=2)

    elif name == "update_task":
        task_id = arguments["task_id"]
        # Update title/description
        patch = {}
        if "title" in arguments:
            patch["title"] = arguments["title"]
        if "description" in arguments:
            patch["description"] = arguments["description"]
        if patch:
            _api("PATCH", f"/tasks/{task_id}", patch)

        # Update status
        if "status" in arguments:
            _api("PATCH", f"/tasks/{task_id}/status", {"status": arguments["status"]})

        task = _api("GET", f"/tasks/{task_id}")
        return (
            f"Updated task '{task['title']}' "
            f"(status: {task['status']}, agent: {task.get('agent_id') or 'unassigned'})"
        )

    elif name == "complete_task":
        _api("PATCH", f"/tasks/{arguments['task_id']}/status", {"status": "completed"})
        task = _api("GET", f"/tasks/{arguments['task_id']}")
        return f"Completed task: {task['title']}"

    elif name == "list_subtasks":
        parent_id = arguments["parent_task_id"]
        resp = _api("GET", "/tasks", {"parent_task_id": parent_id})
        tasks = resp.get("tasks", [])
        # Filter client-side since API may not support parent_task_id filter
        subtasks = [t for t in tasks if t.get("parent_task_id") == parent_id]
        if not subtasks:
            return "No subtasks found."
        lines = [
            f"- [{t['status']}] {t['title']} (id: {t['id']})" for t in subtasks
        ]
        return f"{len(subtasks)} subtask(s):\n" + "\n".join(lines)

    elif name == "list_procedures":
        procedures = _api("GET", "/procedures")
        if not procedures:
            return "No procedures found."
        lines = []
        for p in procedures:
            desc = p.get("description") or ""
            lines.append(f"- {p['name']}: {desc}")
        return f"{len(procedures)} procedure(s):\n" + "\n".join(lines)

    elif name == "run_procedure":
        proc_name = arguments["name"]
        body = {
            "agent_id": arguments.get("agent_id", AGENT_ID or None),
            "inputs": arguments.get("inputs", {}),
        }
        task = _api("POST", f"/procedures/{proc_name}/run", body)
        return (
            f"Started procedure '{proc_name}' "
            f"(task id: {task['id']}, agent: {task.get('agent_id') or 'unassigned'})"
        )

    elif name == "create_schedule":
        body = {
            "name": arguments["name"],
            "cron": arguments["cron"],
            "title": arguments["title"],
            "agent_id": arguments.get("agent_id", AGENT_ID or None),
        }
        if "description" in arguments:
            body["description"] = arguments["description"]

        schedule = _api("POST", "/schedules", body)
        return (
            f"Created schedule '{schedule['name']}' "
            f"(id: {schedule['id']}, cron: {schedule['cron']}, "
            f"agent: {schedule.get('agent_id') or 'unassigned'})"
        )

    elif name == "list_schedules":
        params = {}
        if "agent_id" in arguments:
            params["agent_id"] = arguments["agent_id"]

        schedules = _api("GET", "/schedules", params)
        if not schedules:
            return "No schedules found."
        lines = []
        for s in schedules:
            status = "enabled" if s.get("enabled") else "disabled"
            lines.append(
                f"- [{status}] {s['name']} (id: {s['id']}, "
                f"cron: {s['cron']}, agent: {s.get('agent_id')}, "
                f"runs: {s.get('run_count', 0)})"
            )
        return f"{len(schedules)} schedule(s):\n" + "\n".join(lines)

    elif name == "delete_schedule":
        _api("DELETE", f"/schedules/{arguments['schedule_id']}")
        return f"Deleted schedule {arguments['schedule_id']}"

    else:
        return f"Unknown tool: {name}"


# ---------------------------------------------------------------------------
# MCP stdio protocol
# ---------------------------------------------------------------------------

def _read_message() -> dict | None:
    """Read a JSON-RPC message from stdin."""
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line.strip())


def _write_message(msg: dict):
    """Write a JSON-RPC message to stdout."""
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.buffer.flush()


def _response(id, result):
    return {"jsonrpc": "2.0", "id": id, "result": result}


def _error_response(id, code, message):
    return {"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}


def main():
    """Run the MCP stdio server loop."""
    while True:
        msg = _read_message()
        if msg is None:
            break

        method = msg.get("method", "")
        msg_id = msg.get("id")
        params = msg.get("params", {})

        if method == "initialize":
            _write_message(
                _response(
                    msg_id,
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {
                            "name": "xpressclaw-tasks",
                            "version": "0.1.0",
                        },
                    },
                )
            )

        elif method == "notifications/initialized":
            pass  # no response needed

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
