"""MCP stdio server for xpressclaw Android device control.

Exposes Android control as MCP tools that proxy to the xpressclaw server's
`/v1/android/*` HTTP endpoints — which drive the device via adb_client,
host-side. No adb inside the container (that was the scrcpy-mcp trap). The
screenshot tool returns MCP *image* content so the agent can see the screen.

Environment variables:
  XPRESSCLAW_URL — Base URL of the xpressclaw server
                   (default: http://host.docker.internal:8935)
"""

import base64
import json
import os
import sys

import httpx

BASE_URL = os.environ.get(
    "XPRESSCLAW_URL",
    f"http://host.docker.internal:{os.environ.get('XPRESSCLAW_PORT', '8935')}",
)

TOOLS = [
    {
        "name": "android_screenshot",
        "description": (
            "Capture the current Android screen as an image. Use this when you "
            "need to SEE visual content (photos, icons, layout). To find WHERE "
            "to tap, prefer android_screen_map — it gives exact coordinates; do "
            "not read coordinates off this image. Apps that set FLAG_SECURE "
            "(e.g. authenticators, banking) return a black image — use "
            "android_screen_map instead, it can still read them."
        ),
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "android_screen_map",
        "description": (
            "PRIMARY way to perceive the screen: returns a compact list of "
            "interactable/labeled elements with their text, content-description, "
            "and exact pixel bounds + center. Call this first to decide what to "
            "tap, then act with android_tap_text or android_tap using the "
            "coordinates it gives. Much smaller and more reliable than a "
            "screenshot. After acting, call this again to confirm the result — "
            "apps animate and dialogs appear, so never assume the screen state "
            "from a prior step."
        ),
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "android_tap_text",
        "description": (
            "Find a UI element by its visible text or content-description and tap "
            "its center. PREFER THIS over android_tap with raw coordinates — it "
            "resolves the element's true position from the accessibility tree "
            "instead of guessing pixels from the screenshot."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Visible text or content-description of the element",
                }
            },
            "required": ["label"],
        },
    },
    {
        "name": "android_tap",
        "description": "Tap absolute device pixel coordinates. Prefer android_tap_text when possible.",
        "inputSchema": {
            "type": "object",
            "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}},
            "required": ["x", "y"],
        },
    },
    {
        "name": "android_swipe",
        "description": "Swipe from (x1,y1) to (x2,y2) over `ms` milliseconds — e.g. to scroll.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "x1": {"type": "integer"},
                "y1": {"type": "integer"},
                "x2": {"type": "integer"},
                "y2": {"type": "integer"},
                "ms": {"type": "integer", "default": 300},
            },
            "required": ["x1", "y1", "x2", "y2"],
        },
    },
    {
        "name": "android_type",
        "description": "Type text into the focused field. Tap the field to focus it first.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "android_key",
        "description": "Send a key event, e.g. KEYCODE_BACK, KEYCODE_HOME, KEYCODE_ENTER, KEYCODE_APP_SWITCH.",
        "inputSchema": {
            "type": "object",
            "properties": {"key": {"type": "string"}},
            "required": ["key"],
        },
    },
    {
        "name": "android_long_press",
        "description": "Long-press (touch and hold) at a coordinate — e.g. to open a context menu.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "x": {"type": "integer"},
                "y": {"type": "integer"},
                "ms": {"type": "integer", "default": 600},
            },
            "required": ["x", "y"],
        },
    },
    {
        "name": "android_open_app",
        "description": (
            "Launch an app by its package name (e.g. com.android.settings). PREFER "
            "THIS over tapping a home-screen icon — it's far more reliable."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {"package": {"type": "string"}},
            "required": ["package"],
        },
    },
    {
        "name": "android_dump",
        "description": "Dump the UI accessibility tree (uiautomator XML) to find element text and bounds.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


# One client for the process lifetime: it keeps the HTTP connection pool warm
# across tool calls instead of opening (and tearing down) a fresh TCP connection
# every time. This stdio server is long-lived, so the client is never closed.
_client = httpx.Client(timeout=30.0)


def _call(method: str, path: str, payload=None) -> httpx.Response:
    if method == "GET":
        resp = _client.get(f"{BASE_URL}{path}")
    else:
        resp = _client.post(f"{BASE_URL}{path}", json=payload or {})
    resp.raise_for_status()
    return resp


def _text(s: str):
    return [{"type": "text", "text": s}]


def call_tool(name: str, args: dict):
    """Return MCP content blocks for a tool call."""
    if name == "android_screenshot":
        resp = _call("GET", "/v1/android/screenshot")
        b64 = base64.b64encode(resp.content).decode("ascii")
        mime = resp.headers.get("content-type", "image/jpeg")
        return [{"type": "image", "data": b64, "mimeType": mime}]
    if name == "android_screen_map":
        j = _call("GET", "/v1/android/elements").json()
        els = j.get("elements", [])
        return _text(json.dumps(els, separators=(",", ":")))
    if name == "android_tap_text":
        j = _call("POST", "/v1/android/tap-text", {"label": args["label"]}).json()
        return _text(f"tapped '{args['label']}' at ({j.get('x')}, {j.get('y')})")
    if name == "android_tap":
        _call("POST", "/v1/android/tap", {"x": args["x"], "y": args["y"]})
        return _text(f"tapped ({args['x']}, {args['y']})")
    if name == "android_swipe":
        _call(
            "POST",
            "/v1/android/swipe",
            {
                "x1": args["x1"],
                "y1": args["y1"],
                "x2": args["x2"],
                "y2": args["y2"],
                "ms": args.get("ms", 300),
            },
        )
        return _text("swiped")
    if name == "android_type":
        _call("POST", "/v1/android/input-text", {"text": args["text"]})
        return _text("typed text")
    if name == "android_key":
        _call("POST", "/v1/android/key", {"key": args["key"]})
        return _text(f"sent {args['key']}")
    if name == "android_long_press":
        _call(
            "POST",
            "/v1/android/long-press",
            {"x": args["x"], "y": args["y"], "ms": args.get("ms", 600)},
        )
        return _text(f"long-pressed ({args['x']}, {args['y']})")
    if name == "android_open_app":
        _call("POST", "/v1/android/open-app", {"package": args["package"]})
        return _text(f"launched {args['package']}")
    if name == "android_dump":
        return _text(_call("GET", "/v1/android/dump").text)
    raise ValueError(f"unknown tool: {name}")


# --- MCP stdio protocol (newline-delimited JSON) ---

def _read_message():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line.strip())


def _write_message(obj: dict):
    sys.stdout.write(json.dumps(obj) + "\n")
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
                        "serverInfo": {"name": "android", "version": "0.1.0"},
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
                content = call_tool(tool_name, arguments)
                _write_message(_response(msg_id, {"content": content, "isError": False}))
            except Exception as e:  # noqa: BLE001 — report any failure to the agent
                _write_message(
                    _response(
                        msg_id,
                        {"content": _text(f"Error: {e}"), "isError": True},
                    )
                )
        elif method == "notifications/cancelled":
            pass
        else:
            if msg_id is not None:
                _write_message(_error_response(msg_id, -32601, f"method not found: {method}"))


if __name__ == "__main__":
    main()
