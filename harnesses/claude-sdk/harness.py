"""Claude Agent SDK harness.

Runs a Claude agent using the official claude-agent-sdk package.
The agent executes tool calls (filesystem, shell, etc.) inside the container
via the workspace volume mount at /workspace.

Environment variables:
  ANTHROPIC_API_KEY — API key for Anthropic (if using cloud Anthropic)
  OPENAI_BASE_URL — OpenAI-compatible endpoint URL (if using local models)
  LLM_MODEL        — Model to use (default: claude-sonnet-4-20250514)
  WORKSPACE_DIR    — Agent workspace (default: /workspace)
"""

import json
import os
import sys

sys.path.insert(0, "/app")

try:
    from openai import OpenAI
except ImportError:
    OpenAI = None

from claude_agent_sdk import Agent, AgentConfig, ToolConfig
from server import BaseHarness, logger, AGENT_ID, AGENT_NAME

LLM_MODEL = os.environ.get("LLM_MODEL", "claude-sonnet-4-20250514")
WORKSPACE_DIR = os.environ.get("WORKSPACE_DIR", "/workspace")
OPENAI_BASE_URL = os.environ.get("OPENAI_BASE_URL")
ANTHROPIC_API_KEY = os.environ.get("ANTHROPIC_API_KEY")


class ClaudeSdkHarness(BaseHarness):
    """Runs a Claude Agent SDK agent per request."""

    async def complete(
        self,
        messages: list[dict],
        model: str,
        temperature: float,
        max_tokens: int,
    ) -> str:
        # If OPENAI_BASE_URL is set, use OpenAI SDK to call the xpressclaw server
        if OPENAI_BASE_URL and OpenAI:
            logger.info("Using OpenAI-compatible endpoint: %s", OPENAI_BASE_URL)
            client = OpenAI(base_url=OPENAI_BASE_URL, api_key="not-needed")
            try:
                response = client.chat.completions.create(
                    model=model,
                    messages=messages,
                    temperature=temperature,
                    max_tokens=max_tokens,
                )
                return response.choices[0].message.content or ""
            except Exception as e:
                logger.error("OpenAI API call failed: %s", e)
                raise

        # Otherwise, use Claude SDK (for cloud Anthropic)
        # Extract system prompt and user messages
        system_prompt = ""
        user_messages = []
        for msg in messages:
            if msg["role"] == "system":
                system_prompt = msg["content"]
            else:
                user_messages.append(msg)

        # Get the last user message as the task
        task = ""
        for msg in reversed(user_messages):
            if msg["role"] == "user":
                task = msg["content"]
                break

        if not task:
            return "No task provided."

        use_model = model if model != AGENT_NAME else LLM_MODEL

        # Configure the agent
        config = AgentConfig(
            model=use_model,
            system_prompt=system_prompt or f"You are {AGENT_NAME}, an AI assistant.",
            max_tokens=max_tokens,
            tools=ToolConfig(
                filesystem={"root": WORKSPACE_DIR},
                shell={"enabled": True},
            ),
        )

        agent = Agent(config)

        logger.info(
            "running claude agent: model=%s task_len=%d workspace=%s",
            use_model, len(task), WORKSPACE_DIR,
        )

        result = await agent.run(task)

        return result.text if hasattr(result, "text") else str(result)


if __name__ == "__main__":
    ClaudeSdkHarness().run()
