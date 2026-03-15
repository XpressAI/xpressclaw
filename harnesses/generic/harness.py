"""Generic harness — minimal LLM proxy.

Forwards chat completion requests to a configured LLM API endpoint.
Supports the xpressclaw server's built-in /v1/ router or any OpenAI-compatible API.

Environment variables:
  LLM_BASE_URL — Base URL for the LLM API (default: http://host.docker.internal:8935/v1)
  LLM_API_KEY  — API key for the LLM API (default: none)
  LLM_MODEL    — Override model name (default: use request model)
"""

import os
import sys

# Add base harness to path
sys.path.insert(0, "/app")

import httpx
from server import BaseHarness, logger, AGENT_ID

LLM_BASE_URL = os.environ.get("LLM_BASE_URL", "http://host.docker.internal:8935/v1")
LLM_API_KEY = os.environ.get("LLM_API_KEY", "")
LLM_MODEL = os.environ.get("LLM_MODEL", "")


class GenericHarness(BaseHarness):
    """Proxies requests to an OpenAI-compatible LLM API."""

    def __init__(self):
        super().__init__()
        headers = {"Content-Type": "application/json"}
        if LLM_API_KEY:
            headers["Authorization"] = f"Bearer {LLM_API_KEY}"
        self.client = httpx.AsyncClient(
            base_url=LLM_BASE_URL,
            headers=headers,
            timeout=300.0,
        )
        logger.info("generic harness: LLM_BASE_URL=%s", LLM_BASE_URL)

    async def complete(
        self,
        messages: list[dict],
        model: str,
        temperature: float,
        max_tokens: int,
    ) -> str:
        payload = {
            "model": LLM_MODEL or model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stream": False,
        }

        resp = await self.client.post("/chat/completions", json=payload)
        resp.raise_for_status()
        data = resp.json()

        choices = data.get("choices", [])
        if not choices:
            raise ValueError("LLM returned no choices")

        return choices[0]["message"]["content"]


if __name__ == "__main__":
    GenericHarness().run()
