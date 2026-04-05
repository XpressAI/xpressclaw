"""Base harness server — OpenAI-compatible HTTP server skeleton.

Every harness image inherits from this base and overrides the `complete()` method.
The server exposes:
  POST /v1/chat/completions  — OpenAI chat completion
  GET  /v1/models            — available models
  GET  /health               — health check
"""

import json
import os
import time
import logging
from abc import ABC, abstractmethod
from typing import Optional
from uuid import uuid4

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, StreamingResponse

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
)
logger = logging.getLogger("harness")

# Environment injected by the xpressclaw server
AGENT_ID = os.environ.get("XPRESSCLAW_AGENT_ID", os.environ.get("AGENT_ID", "unknown"))
AGENT_NAME = os.environ.get("AGENT_NAME", AGENT_ID)
AGENT_BACKEND = os.environ.get("AGENT_BACKEND", "generic")
AGENT_CONFIG = os.environ.get("AGENT_CONFIG", "{}")


# Shared cancel flag — written by the /v1/cancel endpoint,
# read by MCP tool handlers to stop at the next tool call.
_workdir = os.environ.get("WORKSPACE_DIR", os.path.expanduser("~/.xpressclaw"))
CANCEL_FLAG = os.path.join(_workdir, ".cancel")


class BaseHarness(ABC):
    """Override `complete()` to implement an agent backend."""

    def __init__(self):
        self.app = FastAPI(title=f"xpressclaw-harness-{AGENT_BACKEND}")
        self._register_routes()

    def _register_routes(self):
        @self.app.get("/health")
        async def health():
            return {"status": "ok", "agent_id": AGENT_ID, "backend": AGENT_BACKEND}

        @self.app.post("/v1/cancel")
        async def cancel():
            """Set the cancel flag so MCP tools stop at the next call."""
            with open(CANCEL_FLAG, "w") as f:
                f.write("1")
            logger.info("cancel requested — will stop at next tool call")
            return {"status": "cancelled"}

        @self.app.get("/v1/models")
        async def models():
            return {
                "object": "list",
                "data": [
                    {
                        "id": AGENT_NAME,
                        "object": "model",
                        "created": 0,
                        "owned_by": "xpressclaw",
                    }
                ],
            }

        @self.app.post("/v1/chat/completions")
        async def chat_completions(request: Request):
            data = await request.json()
            messages = data.get("messages", [])
            model = data.get("model", AGENT_NAME)
            temperature = data.get("temperature", 0.7)
            max_tokens = data.get("max_tokens", 4096)
            stream = data.get("stream", False)

            if stream:
                return StreamingResponse(
                    self._stream_response(messages, model, temperature, max_tokens),
                    media_type="text/event-stream",
                )

            try:
                content = await self.complete(messages, model, temperature, max_tokens)
            except Exception as e:
                logger.exception("completion failed")
                return JSONResponse(
                    status_code=500,
                    content={"error": {"message": str(e), "type": "server_error"}},
                )

            return {
                "id": f"chatcmpl-{uuid4().hex[:16]}",
                "object": "chat.completion",
                "created": int(time.time()),
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": content},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
            }

    async def _stream_response(
        self,
        messages: list,
        model: str,
        temperature: float,
        max_tokens: int,
    ):
        conv_id = uuid4().hex[:16]

        def chunk(delta: dict, finish_reason: Optional[str] = None) -> str:
            payload = {
                "id": f"chatcmpl-{conv_id}",
                "object": "chat.completion.chunk",
                "created": int(time.time()),
                "model": model,
                "choices": [
                    {"index": 0, "delta": delta, "finish_reason": finish_reason}
                ],
            }
            return f"data: {json.dumps(payload)}\n\n"

        try:
            content = await self.complete(messages, model, temperature, max_tokens)
            # Send content in one chunk (subclasses can override for true streaming)
            yield chunk({"role": "assistant", "content": content})
            yield chunk({}, "stop")
            yield "data: [DONE]\n\n"
        except Exception as e:
            logger.exception("streaming completion failed")
            error_payload = {"error": {"message": str(e), "type": "server_error"}}
            yield f"data: {json.dumps(error_payload)}\n\n"

    @abstractmethod
    async def complete(
        self,
        messages: list[dict],
        model: str,
        temperature: float,
        max_tokens: int,
    ) -> str:
        """Run the agent and return the assistant response text."""
        ...

    def run(self, host: str = "0.0.0.0", port: int = 8080):
        import uvicorn

        # Clear stale cancel flag from previous runs
        if os.path.exists(CANCEL_FLAG):
            os.remove(CANCEL_FLAG)

        logger.info(
            "starting harness: agent_id=%s backend=%s on %s:%d",
            AGENT_ID, AGENT_BACKEND, host, port,
        )
        uvicorn.run(self.app, host=host, port=port, log_level="info")
