"""Xaibo framework harness.

Runs a Xaibo agent with OpenAI-compatible chat completions interface.
Agent definitions live in /workspace/agents/ (mounted from host).
Uses Xaibo's built-in OpenAI adapter pattern from the enterprise platform.

Environment variables:
  AGENT_DIR       — Directory containing xaibo agent configs (default: /workspace/agents)
  LLM_BASE_URL    — LLM API URL for xaibo's relay module (default: http://host.docker.internal:8935/v1)
  LLM_API_KEY     — API key for the LLM
  LLM_MODEL       — Default model for the agent
"""

import json
import os
import sys
import time
from uuid import uuid4

sys.path.insert(0, "/app")

from server import BaseHarness, logger, AGENT_ID, AGENT_NAME

AGENT_DIR = os.environ.get("AGENT_DIR", "/workspace/agents")
LLM_BASE_URL = os.environ.get("LLM_BASE_URL", "http://host.docker.internal:8935/v1")
LLM_API_KEY = os.environ.get("LLM_API_KEY", "")
LLM_MODEL = os.environ.get("LLM_MODEL", "")


class XaiboHarness(BaseHarness):
    """Wraps a Xaibo agent behind the OpenAI-compat interface."""

    def __init__(self):
        super().__init__()
        self._xaibo = None

    def _get_xaibo(self):
        """Lazy-initialize Xaibo with agent configs from AGENT_DIR."""
        if self._xaibo is not None:
            return self._xaibo

        from xaibo import Xaibo

        os.environ.setdefault("OPENAI_BASE_URL", LLM_BASE_URL)
        if LLM_API_KEY:
            os.environ.setdefault("OPENAI_API_KEY", LLM_API_KEY)

        self._xaibo = Xaibo(agent_dir=AGENT_DIR)
        agents = self._xaibo.list_agents()
        logger.info("xaibo initialized: agents=%s from %s", agents, AGENT_DIR)
        return self._xaibo

    async def complete(
        self,
        messages: list[dict],
        model: str,
        temperature: float,
        max_tokens: int,
    ) -> str:
        xaibo = self._get_xaibo()

        # Resolve agent ID — model field may be "agent_name" or "agent_name/entry_point"
        agent_id = model
        entry_point = "__entry__"
        if "/" in agent_id:
            agent_id, entry_point = agent_id.split("/", 1)

        # If model doesn't match an agent, use first available
        agents = xaibo.list_agents()
        if agent_id not in agents:
            if agents:
                agent_id = agents[0]
                logger.info("model %s not found, using agent %s", model, agent_id)
            else:
                raise ValueError("no xaibo agents configured")

        # Build conversation from messages
        from xaibo.primitives.modules.conversation.conversation import SimpleConversation

        conversation = SimpleConversation.from_openai_messages(messages)

        from xaibo import ConfigOverrides, ExchangeConfig

        agent = xaibo.get_agent_with(
            agent_id,
            ConfigOverrides(
                instances={"__conversation_history__": conversation},
                exchange=[
                    ExchangeConfig(
                        protocol="ConversationHistoryProtocol",
                        provider="__conversation_history__",
                    )
                ],
            ),
        )

        # Extract last user message
        last_user_msg = ""
        for msg in reversed(messages):
            if msg["role"] == "user":
                last_user_msg = msg["content"]
                break

        logger.info(
            "running xaibo agent=%s entry=%s msg_len=%d",
            agent_id, entry_point, len(last_user_msg),
        )

        result = await agent.handle_text(last_user_msg, entry_point=entry_point)

        return result.text if hasattr(result, "text") else str(result)


if __name__ == "__main__":
    XaiboHarness().run()
