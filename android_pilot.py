#!/usr/bin/env python3
"""
android_pilot.py — minimal standalone agent that drives an Android device
via ADB, with a vision-capable LLM choosing actions.

No xpressclaw, no MCP server. Just adb subprocess + an OpenAI-compatible
API. Designed for a "does the agentic loop work end-to-end?" demo.

Setup:
    uv pip install openai          # or: .venv/bin/pip install openai
    adb devices                    # make sure the device is visible

Usage:
    export OPENAI_API_KEY=sk-...
    export OPENAI_BASE_URL=https://your.endpoint/v1
    export MODEL=qwen3-vl-plus
    python android_pilot.py "open Settings and screenshot the home"

All three can also be passed as CLI flags (--base-url / --model). The
default base URL is the DashScope OpenAI-compatible endpoint.
"""

import argparse
import base64
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

from dotenv import load_dotenv
from openai import OpenAI

load_dotenv()  # auto-load ./.env


# ────────────────────────────── Trace logging ──────────────────────────────

def _slug(s: str, n: int = 40) -> str:
    return re.sub(r"[^a-z0-9]+", "-", s.lower())[:n].strip("-") or "run"

def _elide_images(messages):
    """Replace base64 image_url data with a short placeholder for log readability."""
    cleaned = []
    for m in messages:
        c = m.get("content")
        if isinstance(c, list):
            new_c = []
            for part in c:
                if isinstance(part, dict) and part.get("type") == "image_url":
                    url = part.get("image_url", {}).get("url", "")
                    if url.startswith("data:image/"):
                        # Replace with size info only.
                        b64 = url.split(",", 1)[1] if "," in url else ""
                        approx_bytes = (len(b64) * 3) // 4
                        new_c.append({"type": "image_url",
                                      "image_url": {"url": f"<image:{approx_bytes}B>"}})
                        continue
                new_c.append(part)
            cleaned.append({**m, "content": new_c})
        else:
            cleaned.append(m)
    return cleaned


class RunLogger:
    """Writes a per-run directory with trace.jsonl, summary.json, and screenshots/."""

    def __init__(self, base_dir: Path, task: str, model: str, base_url: str):
        ts = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
        self.dir = base_dir / f"{ts}_{_slug(task)}"
        (self.dir / "screenshots").mkdir(parents=True, exist_ok=True)
        self.trace_path = self.dir / "trace.jsonl"
        self.summary_path = self.dir / "summary.json"
        self.summary = {
            "task": task,
            "model": model,
            "base_url": base_url,
            "started_at": dt.datetime.now().isoformat(),
            "finished_at": None,
            "total_steps": 0,
            "total_latency_s": 0.0,
            "total_prompt_tokens": 0,
            "total_completion_tokens": 0,
            "total_reasoning_chars": 0,
            "final_status": None,
            "final_summary": None,
        }

    def screenshot_path(self, step: int) -> Path:
        return self.dir / "screenshots" / f"step-{step:03d}.png"

    def log_step(self, step: int, request_messages, response, tool_executions, latency_s: float):
        msg = response.choices[0].message
        reasoning = getattr(msg, "reasoning_content", None) or ""
        usage = response.usage
        record = {
            "step": step,
            "ts": dt.datetime.now().isoformat(),
            "latency_s": round(latency_s, 3),
            "request": {
                "model": response.model,
                "messages": _elide_images(request_messages),
            },
            "response": {
                "content": msg.content,
                "reasoning_content": reasoning or None,
                "tool_calls": [
                    {"id": tc.id, "name": tc.function.name, "arguments": tc.function.arguments}
                    for tc in (msg.tool_calls or [])
                ],
                "finish_reason": response.choices[0].finish_reason,
                "usage": {
                    "prompt_tokens": usage.prompt_tokens,
                    "completion_tokens": usage.completion_tokens,
                    "total_tokens": usage.total_tokens,
                } if usage else None,
            },
            "tool_executions": tool_executions,
        }
        with self.trace_path.open("a") as f:
            f.write(json.dumps(record) + "\n")

        # Roll up summary stats.
        self.summary["total_steps"] = step
        self.summary["total_latency_s"] = round(
            self.summary["total_latency_s"] + latency_s, 3)
        if usage:
            self.summary["total_prompt_tokens"] += usage.prompt_tokens
            self.summary["total_completion_tokens"] += usage.completion_tokens
        self.summary["total_reasoning_chars"] += len(reasoning)

    def finalize(self, status: str, final_summary: str | None = None):
        self.summary["finished_at"] = dt.datetime.now().isoformat()
        self.summary["final_status"] = status
        self.summary["final_summary"] = final_summary
        self.summary_path.write_text(json.dumps(self.summary, indent=2))


# ───────────────────────── ADB primitives ─────────────────────────

def adb_screenshot() -> bytes:
    return subprocess.run(
        ["adb", "exec-out", "screencap", "-p"],
        capture_output=True, check=True,
    ).stdout

def adb_tap(x: int, y: int) -> str:
    subprocess.run(["adb", "shell", "input", "tap", str(x), str(y)], check=True)
    return f"tapped ({x},{y})"

def adb_swipe(x1: int, y1: int, x2: int, y2: int, duration_ms: int = 300) -> str:
    subprocess.run(
        ["adb", "shell", "input", "swipe",
         str(x1), str(y1), str(x2), str(y2), str(duration_ms)],
        check=True,
    )
    return f"swiped ({x1},{y1})→({x2},{y2}) in {duration_ms}ms"

def adb_input_text(text: str) -> str:
    # adb shell input text needs %s for spaces and shell-escaping for specials
    safe = text.replace(" ", "%s")
    safe = "".join("\\" + c if c in "()<>|;&*\\?$#'\"`" else c for c in safe)
    subprocess.run(["adb", "shell", "input", "text", safe], check=True)
    return f"typed: {text!r}"

def adb_key_event(keycode: str) -> str:
    subprocess.run(["adb", "shell", "input", "keyevent", str(keycode)], check=True)
    return f"key: {keycode}"

def adb_app_start(package: str) -> str:
    subprocess.run(
        ["adb", "shell", "monkey", "-p", package,
         "-c", "android.intent.category.LAUNCHER", "1"],
        capture_output=True, check=True,
    )
    return f"launched: {package}"

_BOUNDS_RE = re.compile(r"\[(\d+),(\d+)\]\[(\d+),(\d+)\]")

def _parse_bounds(s: str):
    m = _BOUNDS_RE.match(s)
    return tuple(map(int, m.groups())) if m else None

def adb_ui_dump() -> str:
    """Dump the current UI hierarchy as XML."""
    subprocess.run(["adb", "shell", "uiautomator", "dump", "/sdcard/window_dump.xml"],
                   check=True, capture_output=True)
    out = subprocess.run(["adb", "exec-out", "cat", "/sdcard/window_dump.xml"],
                         capture_output=True, check=True)
    return out.stdout.decode("utf-8", errors="replace")

def adb_tap_text(label: str) -> str:
    """Find a node by text or content-desc and tap its center. Deterministic — no vision."""
    xml = adb_ui_dump()
    try:
        root = ET.fromstring(xml)
    except ET.ParseError as e:
        return f"tap_text: failed to parse UI dump: {e}"
    label_l = label.lower()
    candidates = []  # (score, matched_string, bounds)
    for node in root.iter():
        text = (node.get("text") or "").strip()
        desc = (node.get("content-desc") or "").strip()
        bounds_s = node.get("bounds") or ""
        bounds = _parse_bounds(bounds_s)
        if not bounds:
            continue
        score = None
        if text and text.lower() == label_l:
            score = 0  # exact text match
        elif desc and desc.lower() == label_l:
            score = 0
        elif text and label_l in text.lower():
            score = 1  # substring
        elif desc and label_l in desc.lower():
            score = 1
        if score is not None:
            candidates.append((score, text or desc, bounds))
    if not candidates:
        return f"tap_text: no element matched {label!r}"
    candidates.sort(key=lambda c: c[0])
    _, matched, (x1, y1, x2, y2) = candidates[0]
    cx, cy = (x1 + x2) // 2, (y1 + y2) // 2
    subprocess.run(["adb", "shell", "input", "tap", str(cx), str(cy)], check=True)
    return f"tap_text({label!r}) → matched {matched!r} at ({cx},{cy})"


# ───────────────────── Tool schemas (OpenAI format) ─────────────────────

TOOLS = [
    {"type": "function", "function": {
        "name": "screenshot",
        "description": "Capture the current screen. Returns the image to you in the next message.",
        "parameters": {"type": "object", "properties": {}},
    }},
    {"type": "function", "function": {
        "name": "tap",
        "description": "Tap at the given screen coordinates. Prefer `tap_text` for anything with a visible text label — coordinate-based tap is brittle on this model.",
        "parameters": {
            "type": "object",
            "properties": {
                "x": {"type": "integer", "description": "X pixel"},
                "y": {"type": "integer", "description": "Y pixel"},
            },
            "required": ["x", "y"],
        },
    }},
    {"type": "function", "function": {
        "name": "tap_text",
        "description": "Tap a UI element by its visible text or content-description. Uses the accessibility tree to find exact coordinates — DETERMINISTIC, does not rely on vision. Always prefer this over `tap(x,y)` when the target has a text label or icon description.",
        "parameters": {
            "type": "object",
            "properties": {
                "label": {"type": "string", "description": "Exact or substring match of the element's visible text or content-desc"},
            },
            "required": ["label"],
        },
    }},
    {"type": "function", "function": {
        "name": "swipe",
        "description": "Swipe from (x1,y1) to (x2,y2). Useful for scrolling.",
        "parameters": {
            "type": "object",
            "properties": {
                "x1": {"type": "integer"}, "y1": {"type": "integer"},
                "x2": {"type": "integer"}, "y2": {"type": "integer"},
                "duration_ms": {"type": "integer", "default": 300},
            },
            "required": ["x1", "y1", "x2", "y2"],
        },
    }},
    {"type": "function", "function": {
        "name": "input_text",
        "description": "Type into the currently focused text field. Tap first to focus.",
        "parameters": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    }},
    {"type": "function", "function": {
        "name": "key_event",
        "description": "Send a key event. Common: KEYCODE_BACK, KEYCODE_HOME, KEYCODE_ENTER, KEYCODE_MENU.",
        "parameters": {
            "type": "object",
            "properties": {"keycode": {"type": "string"}},
            "required": ["keycode"],
        },
    }},
    {"type": "function", "function": {
        "name": "app_start",
        "description": "Launch an app by package name (e.g. com.android.settings).",
        "parameters": {
            "type": "object",
            "properties": {"package": {"type": "string"}},
            "required": ["package"],
        },
    }},
    {"type": "function", "function": {
        "name": "finish",
        "description": "Call this when the task is complete or you cannot proceed. Include a brief summary.",
        "parameters": {
            "type": "object",
            "properties": {"summary": {"type": "string"}},
            "required": ["summary"],
        },
    }},
]


SYSTEM_PROMPT = """You are an Android pilot. You drive an Android emulator via tool calls.

Workflow:
1. Call `screenshot` first to see the current state.
2. Identify what you want to interact with on the screen.
3. To interact:
   - PREFER `tap_text(label)` for anything with a visible text label
     ("Battery", "Allow", "Sign in"). It uses the accessibility tree
     to find exact coordinates — deterministic, no coordinate guessing.
   - Use coordinate-based `tap(x, y)` only when there's no text label
     (icons without descriptions, custom canvases, image hotspots).
   - Use `swipe`, `input_text`, `key_event`, `app_start` as needed.
4. After any action that changes the screen, call `screenshot` again.
5. When the task is complete, call `finish` with a brief summary.

The device is a Pixel 6 emulator, 1080×2400 pixels. Coordinates in
screenshots match displayed pixels. Never guess coordinates — read them
from the most recent screenshot.

Installed packages on this device (use these exact names with app_start):
  - com.android.settings           (Settings)
  - com.google.android.deskclock   (Clock)
  - com.android.chrome             (Chrome)
  - com.google.android.apps.maps   (Maps)
  - com.google.android.youtube     (YouTube)
  - com.google.android.youtube.music (YT Music)

Tool argument reminders:
  - `tap`: x, y  (integers)
  - `swipe`: x1, y1, x2, y2, duration_ms (integers)
  - `key_event`: keycode (e.g. KEYCODE_HOME, KEYCODE_BACK, KEYCODE_ENTER)
Don't mix up the parameter names. If a tool call fails, read the error
and try again with corrected arguments — don't repeat the same mistake.

Act in small steps. Observe between each step. Don't over-explain."""


# ───────────────────────────── Tool dispatch ─────────────────────────────

def execute_tool(name: str, args: dict, screenshot_path: Path):
    """Returns (text_result, screenshot_bytes_or_None)."""
    if name == "screenshot":
        img = adb_screenshot()
        screenshot_path.write_bytes(img)
        return f"screenshot saved to {screenshot_path} ({len(img)} bytes)", img
    if name == "tap":
        return adb_tap(args["x"], args["y"]), None
    if name == "swipe":
        return adb_swipe(args["x1"], args["y1"], args["x2"], args["y2"],
                         args.get("duration_ms", 300)), None
    if name == "input_text":
        return adb_input_text(args["text"]), None
    if name == "key_event":
        return adb_key_event(args["keycode"]), None
    if name == "app_start":
        return adb_app_start(args["package"]), None
    if name == "tap_text":
        return adb_tap_text(args["label"]), None
    if name == "finish":
        return f"FINISH: {args.get('summary', '')}", None
    return f"unknown tool: {name}", None


# ───────────────────────────── Main loop ─────────────────────────────

def main():
    p = argparse.ArgumentParser()
    p.add_argument("task", help="Natural language task for the agent")
    p.add_argument("--model", default=os.getenv("MODEL", "qwen3-vl-plus"))
    p.add_argument(
        "--base-url",
        default=os.getenv("OPENAI_BASE_URL",
                          "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
    )
    p.add_argument("--max-steps", type=int, default=20)
    p.add_argument("--logs-dir", default="logs/pilot")
    args = p.parse_args()

    api_key = os.getenv("OPENAI_API_KEY")
    if not api_key:
        sys.exit("error: $OPENAI_API_KEY not set")

    # Sanity-check the device
    out = subprocess.run(["adb", "devices"], capture_output=True, text=True, check=True).stdout
    if not any("\tdevice" in line for line in out.splitlines()[1:]):
        sys.exit(f"no adb device attached:\n{out}")

    base_logs = Path(args.logs_dir)
    base_logs.mkdir(parents=True, exist_ok=True)
    runlog = RunLogger(base_logs, args.task, args.model, args.base_url)

    client = OpenAI(base_url=args.base_url, api_key=api_key)

    messages = [
        {"role": "system", "content": SYSTEM_PROMPT},
        {"role": "user", "content": args.task},
    ]

    print(f"[task]  {args.task}")
    print(f"[model] {args.model} via {args.base_url}")
    print(f"[logs]  {runlog.dir}/")
    print()

    final_status, final_summary = "max_steps", None

    try:
        for step in range(1, args.max_steps + 1):
            t0 = time.perf_counter()
            resp = client.chat.completions.create(
                model=args.model,
                messages=messages,
                tools=TOOLS,
                tool_choice="auto",
                max_tokens=2048,  # leaves room for reasoning trace + tool call
            )
            latency = time.perf_counter() - t0
            msg = resp.choices[0].message

            assistant_msg = {"role": "assistant", "content": msg.content or ""}
            if msg.tool_calls:
                assistant_msg["tool_calls"] = [
                    {
                        "id": tc.id,
                        "type": "function",
                        "function": {"name": tc.function.name, "arguments": tc.function.arguments},
                    }
                    for tc in msg.tool_calls
                ]
            # Snapshot the request for logging BEFORE we append the response,
            # so the log shows what the model was actually shown.
            request_messages_snapshot = list(messages)
            messages.append(assistant_msg)

            if msg.content:
                print(f"[{step:02d}] {msg.content}")

            tool_executions = []
            if not msg.tool_calls:
                runlog.log_step(step, request_messages_snapshot, resp,
                                tool_executions, latency)
                final_status, final_summary = "no_tool_calls", msg.content
                print("\n[done] no tool calls — assistant returned a final text response.")
                return

            for tc in msg.tool_calls:
                name = tc.function.name
                try:
                    tool_args = json.loads(tc.function.arguments or "{}")
                except json.JSONDecodeError:
                    tool_args = {}
                print(f"     → {name}({json.dumps(tool_args)})")

                screenshot_path = runlog.screenshot_path(step)
                try:
                    text, img = execute_tool(name, tool_args, screenshot_path)
                except subprocess.CalledProcessError as e:
                    text, img = f"adb error: {e}", None
                except (KeyError, TypeError, ValueError) as e:
                    text, img = (f"tool error: {type(e).__name__}: {e}. "
                                 f"Got args={tool_args}. Re-check the tool schema."), None
                print(f"       {text}")

                tool_executions.append({
                    "name": name,
                    "args": tool_args,
                    "result": text,
                    "screenshot": str(screenshot_path.relative_to(runlog.dir))
                                  if img is not None else None,
                    "latency_s": None,  # adb commands are fast; not separately timed here
                })

                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.id,
                    "content": text,
                })

                if img is not None:
                    # Inject screenshot as a follow-up user message so the model
                    # can actually see it. OpenAI's `tool` role content is text-only,
                    # so the image goes in a separate user-content image block.
                    b64 = base64.b64encode(img).decode()
                    messages.append({
                        "role": "user",
                        "content": [
                            {"type": "text", "text": "[current screen]"},
                            {"type": "image_url",
                             "image_url": {"url": f"data:image/png;base64,{b64}"}},
                        ],
                    })

                if name == "finish":
                    runlog.log_step(step, request_messages_snapshot, resp,
                                    tool_executions, latency)
                    final_status = "finish"
                    final_summary = tool_args.get("summary")
                    return

            runlog.log_step(step, request_messages_snapshot, resp,
                            tool_executions, latency)

        print(f"\n[done] hit max-steps={args.max_steps}")
    finally:
        runlog.finalize(final_status, final_summary)
        print(f"\n[summary]")
        s = runlog.summary
        print(f"  steps:       {s['total_steps']}")
        print(f"  latency:     {s['total_latency_s']}s total")
        print(f"  tokens:      {s['total_prompt_tokens']} prompt + "
              f"{s['total_completion_tokens']} completion")
        print(f"  reasoning:   {s['total_reasoning_chars']} chars")
        print(f"  status:      {s['final_status']}")
        print(f"  trace:       {runlog.trace_path}")
        print(f"  summary:     {runlog.summary_path}")

    print(f"\n[done] hit max-steps={args.max_steps}")


if __name__ == "__main__":
    main()
