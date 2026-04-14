/**
 * Pi extension: xpressclaw local model provider.
 *
 * Registers the xpressclaw LLM proxy (or direct llama-server) as a
 * custom OpenAI-compatible provider so pi can use local models.
 *
 * Environment variables:
 *   XPRESSCLAW_LLM_URL  — base URL (default: http://host.docker.internal:8081/v1)
 *   XPRESSCLAW_LLM_KEY  — API key (default: opensesame)
 *   XPRESSCLAW_MODEL    — model ID (default: local)
 */

import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";

export default function (pi: ExtensionAPI) {
    const baseUrl = process.env.XPRESSCLAW_LLM_URL || "http://host.docker.internal:8081/v1";
    const apiKey = process.env.XPRESSCLAW_LLM_KEY || "opensesame";
    const modelId = process.env.XPRESSCLAW_MODEL || "local";

    pi.registerProvider("xpressclaw", {
        baseUrl,
        apiKey,
        authHeader: true,
        api: "openai-completions",
        models: [
            {
                id: modelId,
                name: "xpressclaw local model",
                reasoning: true,
                input: ["text"],
                cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
                contextWindow: 131072,
                maxTokens: 32768,
                compat: {
                    supportsDeveloperRole: false,
                    maxTokensField: "max_tokens",
                    thinkingFormat: "qwen-chat-template",
                },
            },
        ],
    });
}
