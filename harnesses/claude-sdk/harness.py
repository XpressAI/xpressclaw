"""Claude Agent SDK harness with persistent sessions.

Each agent runs a persistent Claude SDK session. Messages from conversations
are injected as events, and the agent responds within the same session context.
The SDK handles compaction automatically.

Architecture (ADR-021):
- Session persists across messages (context preserved)
- Messages arrive as: SYSTEM: Message in conversation "conv-id" from Sender: content
- Agent responds, response is routed back to the conversation
- Daily session lifecycle: morning start with workspace listing, end-of-day notes

Environment variables:
  ANTHROPIC_API_KEY  — API key (real or placeholder for local models)
  ANTHROPIC_BASE_URL — API base URL (routed through xpressclaw server)
  LLM_MODEL          — Model to use (default: claude-sonnet-4-6)
  WORKSPACE_DIR      — Agent workspace (default: /workspace)
  MCP_SERVERS        — JSON dict of MCP server configs
"""

import asyncio
import json
import os
import queue
import sys
import threading
import time
from datetime import datetime
from uuid import uuid4

sys.path.insert(0, "/app")

from claude_agent_sdk import (
    ClaudeAgentOptions,
    ResultMessage,
    AssistantMessage,
    query,
    list_sessions,
    get_session_info,
)
try:
    from claude_agent_sdk import StreamEvent
except ImportError:
    from claude_agent_sdk.types import StreamEvent

from fastapi import Request
from fastapi.responses import JSONResponse, StreamingResponse
from server import BaseHarness, logger, AGENT_ID, AGENT_NAME

LLM_MODEL = os.environ.get("LLM_MODEL", "claude-sonnet-4-6")
WORKSPACE_DIR = os.environ.get("WORKSPACE_DIR", "/workspace")


def load_mcp_servers() -> dict:
    """Load MCP server configs from the MCP_SERVERS environment variable."""
    raw = os.environ.get("MCP_SERVERS", "")
    if not raw:
        return {}
    try:
        servers = json.loads(raw)
    except json.JSONDecodeError:
        logger.warning("failed to parse MCP_SERVERS env var")
        return {}

    sdk_servers = {}
    for name, config in servers.items():
        server_type = config.get("type", "stdio")
        if server_type == "stdio" and config.get("command"):
            entry: dict = {
                "command": config["command"],
                "args": config.get("args", []),
            }
            if config.get("env"):
                entry["env"] = config["env"]
            sdk_servers[name] = entry
        elif server_type == "sse" and config.get("url"):
            sdk_servers[name] = {"type": "sse", "url": config["url"]}

    logger.info("MCP servers loaded: %s", list(sdk_servers.keys()))
    return sdk_servers


def _build_options(model: str, system_prompt: str, mcp_servers: dict,
                   session_id: str | None = None) -> ClaudeAgentOptions:
    """Build ClaudeAgentOptions with optional session continuation."""
    options = ClaudeAgentOptions(
        model=model,
        system_prompt=system_prompt or f"You are {AGENT_NAME}, an AI assistant.",
        permission_mode="bypassPermissions",
        cwd=WORKSPACE_DIR,
        max_turns=25,
        disallowed_tools=["AskUserQuestion"],
        setting_sources=["project", "user"],
        include_partial_messages=True,
    )
    if mcp_servers:
        options.mcp_servers = mcp_servers
    if session_id:
        options.session_id = session_id
        options.continue_conversation = True
    return options


def _extract_task(messages: list[dict]) -> tuple[str, str]:
    """Extract system prompt and task from OpenAI-format messages."""
    system_prompt = ""
    parts = []
    for msg in messages:
        if msg["role"] == "system":
            system_prompt = msg["content"]
        elif msg["role"] == "user":
            parts.append(f"User: {msg['content']}")
        elif msg["role"] == "assistant":
            parts.append(f"Assistant: {msg['content']}")

    user_messages = [m for m in messages if m["role"] == "user"]
    if len(user_messages) <= 1:
        task = user_messages[-1]["content"] if user_messages else ""
    else:
        task = "\n\n".join(parts) + "\n\nContinue the conversation. Respond to the last user message."

    return system_prompt, task


class ClaudeSdkHarness(BaseHarness):
    """Persistent session agent using Claude Agent SDK."""

    def __init__(self):
        super().__init__()
        self.session_id: str | None = None
        self.mcp_servers = load_mcp_servers()
        self._register_session_routes()

    def _register_session_routes(self):
        """Register the session message endpoint."""

        @self.app.post("/v1/session/send")
        async def session_send(request: Request):
            """Send a message to the agent's persistent session.

            Body: {
                "message": "user's message text",
                "conversation_id": "conv-123",
                "sender_name": "Eduardo",
                "sender_type": "user",  # or "system"
                "system_prompt": "optional system prompt override"
            }

            The message is formatted and injected into the persistent session.
            The agent responds within the same context.
            Streams SSE if Accept: text/event-stream, otherwise returns JSON.
            """
            data = await request.json()
            message = data.get("message", "")
            conversation_id = data.get("conversation_id", "unknown")
            sender_name = data.get("sender_name", "User")
            sender_type = data.get("sender_type", "user")
            system_prompt = data.get("system_prompt", "")
            stream = data.get("stream", True)

            # Format the message as a session event
            if sender_type == "system":
                prompt = f"SYSTEM: {message}"
            else:
                prompt = (
                    f'Message in conversation "{conversation_id}" '
                    f"from {sender_name}:\n{message}"
                )

            model = LLM_MODEL

            if stream:
                return StreamingResponse(
                    self._session_stream(prompt, model, system_prompt),
                    media_type="text/event-stream",
                )
            else:
                content = await self._session_complete(prompt, model, system_prompt)
                return JSONResponse({
                    "content": content,
                    "session_id": self.session_id,
                })

        @self.app.get("/v1/session/info")
        async def session_info():
            """Get current session info."""
            info = None
            if self.session_id:
                try:
                    info = get_session_info(self.session_id, directory=WORKSPACE_DIR)
                except Exception:
                    pass
            return {
                "session_id": self.session_id,
                "agent_id": AGENT_ID,
                "info": {
                    "created_at": info.created_at if info else None,
                    "cwd": info.cwd if info else WORKSPACE_DIR,
                } if info else None,
            }

        @self.app.post("/v1/session/new")
        async def new_session(request: Request):
            """Start a new session (e.g. daily reset)."""
            data = await request.json()
            notes = data.get("notes", "")

            # End current session with notes if provided
            if self.session_id and notes:
                try:
                    await self._session_complete(
                        f"SYSTEM: Session ending. Please write down any important "
                        f"notes for your next session and clean up your workspace.\n"
                        f"Previous notes: {notes}",
                        LLM_MODEL, ""
                    )
                except Exception as e:
                    logger.warning("failed to end session with notes: %s", e)

            # Start fresh session
            self.session_id = None
            self._ensure_session()

            return {"session_id": self.session_id}

    def _ensure_session(self):
        """Ensure we have an active session, creating one if needed."""
        if self.session_id:
            return

        # Check for existing sessions
        try:
            sessions = list_sessions(directory=WORKSPACE_DIR, limit=1)
            if sessions:
                self.session_id = sessions[0].created_at  # Use most recent
                # Actually, we need the session ID not created_at
                # The SDK uses the session's directory name as ID
                # Let's create a new one and let the SDK manage it
                self.session_id = None
        except Exception as e:
            logger.warning("failed to list sessions: %s", e)

        if not self.session_id:
            # Create a new session by running an initial query
            # The SDK will assign a session_id automatically
            logger.info("creating new agent session")

    async def _session_complete(self, prompt: str, model: str,
                                system_prompt: str) -> str:
        """Run a prompt in the persistent session (non-streaming)."""
        use_model = model if model != AGENT_NAME else LLM_MODEL
        options = _build_options(use_model, system_prompt, self.mcp_servers,
                                self.session_id)

        logger.info(
            "session query: model=%s session=%s prompt_len=%d",
            use_model, self.session_id or "new", len(prompt),
        )

        result_text = ""
        async for message in query(prompt=prompt, options=options):
            if isinstance(message, ResultMessage):
                if not self.session_id:
                    # Capture session_id from first query
                    self.session_id = _extract_session_id(message)
                if message.is_error:
                    return f"Agent error: {message.result or 'unknown error'}"
                result_text = message.result or ""
            elif isinstance(message, AssistantMessage):
                if hasattr(message, "content"):
                    for block in message.content:
                        if hasattr(block, "text"):
                            result_text = block.text

        logger.info("session query complete: session=%s result_len=%d",
                     self.session_id, len(result_text))
        return result_text or "No response from agent."

    async def _session_stream(self, prompt: str, model: str,
                              system_prompt: str):
        """Run a prompt in the persistent session with SSE streaming."""
        use_model = model if model != AGENT_NAME else LLM_MODEL
        options = _build_options(use_model, system_prompt, self.mcp_servers,
                                self.session_id)

        logger.info(
            "session stream: model=%s session=%s prompt_len=%d",
            use_model, self.session_id or "new", len(prompt),
        )

        conv_id = uuid4().hex[:16]
        q: queue.Queue[str | None] = queue.Queue()

        def _run_in_thread():
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(
                    _session_query_to_queue(q, conv_id, use_model, prompt,
                                           options, self)
                )
            except Exception as e:
                logger.exception("session query thread failed")
                error_payload = {"error": {"message": str(e), "type": "server_error"}}
                q.put(f"data: {json.dumps(error_payload)}\n\n")
            finally:
                q.put(None)
                try:
                    loop.run_until_complete(loop.shutdown_asyncgens())
                except Exception:
                    pass
                loop.close()

        thread = threading.Thread(target=_run_in_thread, daemon=True)
        thread.start()

        while True:
            try:
                chunk = await asyncio.get_event_loop().run_in_executor(
                    None, q.get, True, 300
                )
            except Exception:
                break
            if chunk is None:
                break
            yield chunk

    # Keep legacy endpoints for backward compatibility (tasks use these)
    async def complete(self, messages, model, temperature, max_tokens) -> str:
        system_prompt, task = _extract_task(messages)
        if not task:
            return "No task provided."
        return await self._session_complete(task, model, system_prompt)

    async def _stream_response(self, messages, model, temperature, max_tokens):
        system_prompt, task = _extract_task(messages)
        if not task:
            yield _sse_chunk(model, {"role": "assistant", "content": "No task provided."}, "stop")
            yield "data: [DONE]\n\n"
            return

        async for chunk in self._session_stream(task, model, system_prompt):
            yield chunk


async def _session_query_to_queue(
    q: "queue.Queue[str | None]",
    conv_id: str,
    model: str,
    prompt: str,
    options: ClaudeAgentOptions,
    harness: ClaudeSdkHarness,
):
    """Run a session query and push SSE chunks to a thread-safe queue."""
    sent_role = False
    in_thinking = False
    in_tool_call = False

    def _send(text: str):
        nonlocal sent_role
        delta = {"content": text}
        if not sent_role:
            delta["role"] = "assistant"
            sent_role = True
        q.put(_sse_chunk_raw(conv_id, model, delta, None))

    try:
        async for message in query(prompt=prompt, options=options):
            if isinstance(message, StreamEvent):
                event = message.event
                event_type = event.get("type", "")

                if event_type == "content_block_start":
                    block = event.get("content_block", {})
                    block_type = block.get("type", "")
                    if block_type == "thinking":
                        in_thinking = True
                        _send("<think>")
                    elif block_type == "tool_use":
                        tool_name = block.get("name", "tool")
                        if "__" in tool_name:
                            tool_name = tool_name.rsplit("__", 1)[-1]
                        _send(f'<tool_call name="{tool_name}">')
                        in_tool_call = True

                elif event_type == "content_block_delta":
                    delta_obj = event.get("delta", {})
                    delta_type = delta_obj.get("type", "")

                    if delta_type == "thinking_delta":
                        text = delta_obj.get("thinking", "")
                        if text:
                            _send(text)
                    elif delta_type == "text_delta":
                        text = delta_obj.get("text", "")
                        if text:
                            _send(text)
                    elif delta_type == "input_json_delta":
                        text = delta_obj.get("partial_json", "")
                        if text:
                            _send(text)

                elif event_type == "content_block_stop":
                    if in_thinking:
                        _send("</think>")
                        in_thinking = False
                    elif in_tool_call:
                        _send("</tool_call>")
                        in_tool_call = False

            elif isinstance(message, AssistantMessage):
                if not sent_role and hasattr(message, "content"):
                    for block in message.content:
                        if hasattr(block, "text") and block.text:
                            _send(block.text)

            elif isinstance(message, ResultMessage):
                logger.info("ResultMessage: is_error=%s result_len=%d",
                            message.is_error, len(message.result or ""))
                # Capture session_id from result
                if not harness.session_id:
                    harness.session_id = _extract_session_id(message)
                if message.is_error:
                    _send(f"Agent error: {message.result or 'unknown error'}")
                elif not sent_role and message.result:
                    _send(message.result)
                q.put(_sse_chunk_raw(conv_id, model, {}, "stop"))
                q.put("data: [DONE]\n\n")
                return

    except Exception as e:
        logger.exception("session query error")
        error_payload = {"error": {"message": str(e), "type": "server_error"}}
        q.put(f"data: {json.dumps(error_payload)}\n\n")

    if not sent_role:
        _send("No response from agent.")
    q.put(_sse_chunk_raw(conv_id, model, {}, "stop"))
    q.put("data: [DONE]\n\n")


def _extract_session_id(message: ResultMessage) -> str | None:
    """Try to extract session_id from a ResultMessage."""
    # The SDK stores session info internally — we can get it from session listing
    try:
        sessions = list_sessions(directory=WORKSPACE_DIR, limit=1)
        if sessions:
            # Return the most recent session's identifier
            # SDK session IDs are typically timestamps or UUIDs
            return sessions[0].created_at
    except Exception:
        pass
    return None


def _sse_chunk_raw(conv_id: str, model: str, delta: dict,
                   finish_reason: str | None) -> str:
    payload = {
        "id": f"chatcmpl-{conv_id}",
        "object": "chat.completion.chunk",
        "created": int(time.time()),
        "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
    }
    return f"data: {json.dumps(payload)}\n\n"


def _sse_chunk(model: str, delta: dict, finish_reason: str | None) -> str:
    return _sse_chunk_raw(uuid4().hex[:16], model, delta, finish_reason)


if __name__ == "__main__":
    ClaudeSdkHarness().run()
