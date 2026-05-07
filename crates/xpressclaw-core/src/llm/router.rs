use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};

/// A boxed stream of chat completion chunks.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>;

/// Deserialize a string that may be null as an empty string.
fn nullable_string<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

/// A chat message in OpenAI format.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, deserialize_with = "nullable_string")]
    pub content: String,
    /// Tool calls requested by the assistant (role=assistant, finish_reason=tool_calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Tool call ID this message is responding to (role=tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning/thinking content from reasoning models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl ChatMessage {
    /// Create a simple text message.
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }

    /// Create a tool result message.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            ..Default::default()
        }
    }
}

/// A tool call from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

fn default_tool_type() -> String {
    "function".into()
}

/// The function being called in a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// Request for chat completion (OpenAI-compatible).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Available tools for the model to call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    /// How the model should choose tools: "auto", "none", or "required".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    /// Maximum tokens for reasoning/thinking. Caps the `<think>` block.
    /// Higher for tasks (e.g. 8192), lower for chat (e.g. 1024).
    /// Serialized as `reasoning_budget_tokens` for llama-server compat.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "reasoning_budget",
        rename = "reasoning_budget_tokens"
    )]
    pub reasoning_budget: Option<i64>,
}

/// Token usage stats.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Tokens used for reasoning/thinking (reasoning models).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
}

/// A choice in the completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: i64,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Response from chat completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Option<Usage>,
}

/// A streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: i64,
    pub delta: ChunkDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Reasoning/thinking content delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Streaming tool call deltas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ChunkToolCall>>,
}

/// A streaming tool call delta from OpenAI format.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkToolCall {
    #[serde(default)]
    pub index: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub call_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<ChunkToolCallFunction>,
}

/// Streaming function call delta.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChunkToolCallFunction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Model info for /v1/models.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub owned_by: String,
}

/// Trait that all LLM providers implement.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    /// Complete a chat request (non-streaming).
    async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse>;

    /// Stream a chat completion as a series of chunks.
    /// Default implementation wraps `chat()` into a single-chunk stream.
    async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let resp = self.chat(request).await?;
        let chunk = ChatCompletionChunk {
            id: resp.id,
            object: "chat.completion.chunk".into(),
            created: resp.created,
            model: resp.model,
            choices: resp
                .choices
                .into_iter()
                .map(|c| ChunkChoice {
                    index: c.index,
                    delta: ChunkDelta {
                        role: Some(c.message.role),
                        content: Some(c.message.content),
                        ..Default::default()
                    },
                    finish_reason: c.finish_reason,
                })
                .collect(),
        };
        Ok(Box::pin(futures_util::stream::once(
            async move { Ok(chunk) },
        )))
    }

    /// List available models.
    fn models(&self) -> Vec<ModelInfo>;

    /// Provider name.
    fn name(&self) -> &str;
}

/// Resolved binding: which provider instance handles a request, and what
/// model name to pass to it.
#[derive(Debug, Clone)]
struct AgentBinding {
    provider_key: String,
    real_model: String,
}

/// Routes LLM requests using per-agent logical model names.
///
/// Each agent declares its own provider/model/api_key/base_url. The router
/// builds one provider instance per unique `(provider_type, api_key, base_url)`
/// combination — many agents can share an instance — and maps each agent's
/// logical name (its `agent.name`) to the provider instance and the *real*
/// model name that should be sent to that provider.
///
/// Harnesses inside agent containers use the agent's name as the model field
/// in their requests (set via `LLM_MODEL=<agent_id>`). The router rewrites
/// the request's `model` field to the real model name before dispatching.
///
/// Real model names are also registered as a fallback so direct callers
/// (e.g. the Anthropic-direct proxy passing through `claude-sonnet-4`) can
/// still resolve. Unknown names error — there is no random "first available
/// provider" fallback.
pub struct LlmRouter {
    /// provider_key → provider instance. Keys are stable strings derived
    /// from `(provider_type, api_key, base_url)`.
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    /// agent_id (logical model name) → binding. Wrapped in RwLock so the
    /// budget manager can re-point an agent at a degraded model at runtime
    /// without rebuilding the entire router.
    agent_bindings: RwLock<HashMap<String, AgentBinding>>,
    /// real model name → provider_key. Lets direct calls with explicit model
    /// names resolve too. Populated alongside `agent_bindings`.
    model_to_provider: RwLock<HashMap<String, String>>,
}

impl Default for LlmRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmRouter {
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            agent_bindings: RwLock::new(HashMap::new()),
            model_to_provider: RwLock::new(HashMap::new()),
        }
    }

    /// Stable key for deduplicating provider instances.
    fn provider_key(provider: &str, api_key: Option<&str>, base_url: Option<&str>) -> String {
        format!(
            "{provider}|{}|{}",
            api_key.unwrap_or(""),
            base_url.unwrap_or("")
        )
    }

    /// Build a fully configured LLM router from the full app config.
    ///
    /// Walks every agent, materializes its provider instance (deduplicating
    /// by `(provider_type, api_key, base_url)`), and registers a binding
    /// from the agent's name to that instance + the agent's real model.
    pub fn build_from_config(config: &Config) -> Self {
        let mut router = Self::new();

        for agent in &config.agents {
            let Some(llm) = agent.llm.as_ref() else {
                continue;
            };
            let Some(provider_type) = llm.provider.as_deref() else {
                continue;
            };
            let Some(real_model) = llm.model.as_deref() else {
                continue;
            };

            let provider_key = Self::provider_key(
                provider_type,
                llm.api_key.as_deref(),
                llm.base_url.as_deref(),
            );

            // Materialize the provider instance once per unique key.
            if !router.providers.contains_key(&provider_key) {
                match router.materialize_provider(provider_type, llm) {
                    Ok(Some(instance)) => {
                        router.providers.insert(provider_key.clone(), instance);
                    }
                    Ok(None) => {
                        // Provider type known but couldn't be built (e.g. missing key).
                        tracing::warn!(
                            agent = %agent.name,
                            provider = provider_type,
                            "skipping agent — provider could not be constructed"
                        );
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent = %agent.name,
                            provider = provider_type,
                            error = %e,
                            "skipping agent — provider construction failed"
                        );
                        continue;
                    }
                }
            }

            router.bind_agent(&agent.name, &provider_key, real_model);
        }

        router
    }

    /// Construct a provider instance for the given config.
    ///
    /// Returns `Ok(None)` if the provider type is recognized but can't be
    /// built with the supplied config (e.g. anthropic without an API key).
    /// Returns `Err` for unrecognized provider types.
    fn materialize_provider(
        &self,
        provider_type: &str,
        llm: &crate::config::AgentLlmConfig,
    ) -> Result<Option<Arc<dyn LlmProvider>>> {
        match provider_type {
            "openai" => Ok(Some(Arc::new(super::openai::OpenAiProvider::new(
                llm.api_key.clone(),
                llm.base_url.clone(),
            )))),
            "anthropic" => match llm.api_key.as_ref() {
                Some(key) => Ok(Some(Arc::new(super::anthropic::AnthropicProvider::new(
                    key.clone(),
                )))),
                None => Ok(None),
            },
            "ollama" => {
                // HTTP proxy to Ollama/vLLM/llama-server.
                let model = llm.model.clone().unwrap_or_default();
                Ok(Some(Arc::new(super::local::LocalProvider::from_config(
                    model,
                    llm.base_url.clone(),
                ))))
            }
            "local" => {
                // Embedded llama.cpp via GGUF file. Only available when the
                // crate is built with the `local-llm` feature.
                #[cfg(feature = "local-llm")]
                {
                    let Some(path) = llm.model_path.as_deref() else {
                        tracing::warn!(
                            "provider=local requires `model_path` pointing to a GGUF file"
                        );
                        return Ok(None);
                    };
                    let model_name = llm.model.clone().unwrap_or_else(|| "local".to_string());
                    match super::llamacpp::LazyLlamaCppProvider::new(
                        std::path::PathBuf::from(path),
                        model_name,
                    ) {
                        Ok(provider) => Ok(Some(Arc::new(provider))),
                        Err(e) => {
                            tracing::warn!(error = %e, "GGUF model not found");
                            Ok(None)
                        }
                    }
                }
                #[cfg(not(feature = "local-llm"))]
                {
                    let _ = llm;
                    tracing::warn!(
                        "provider=local requires the `local-llm` feature; ignoring agent"
                    );
                    Ok(None)
                }
            }
            other => Err(Error::Llm(format!("unknown provider type '{other}'"))),
        }
    }

    /// Register an existing provider under a stable key. Used by tests and
    /// by callers that build providers themselves.
    pub fn register_provider(&mut self, key: &str, provider: Arc<dyn LlmProvider>) {
        self.providers.insert(key.to_string(), provider);
    }

    /// Bind an agent (logical name) to a provider key + real model name.
    ///
    /// Also registers `real_model → provider_key` so direct calls with the
    /// real model name resolve to the same provider.
    pub fn bind_agent(&self, agent_id: &str, provider_key: &str, real_model: &str) {
        if let Ok(mut bindings) = self.agent_bindings.write() {
            bindings.insert(
                agent_id.to_string(),
                AgentBinding {
                    provider_key: provider_key.to_string(),
                    real_model: real_model.to_string(),
                },
            );
        }
        if let Ok(mut models) = self.model_to_provider.write() {
            models.insert(real_model.to_string(), provider_key.to_string());
        }
    }

    /// Re-point an existing agent at a different real model. Returns true
    /// if the agent had an existing binding. Used by budget-driven
    /// degradation: a paused agent gets re-pointed at a cheaper model
    /// without rebuilding the router.
    pub fn set_agent_model(&self, agent_id: &str, real_model: &str) -> bool {
        let provider_key = match self.agent_bindings.read() {
            Ok(bindings) => bindings.get(agent_id).map(|b| b.provider_key.clone()),
            Err(_) => None,
        };
        let Some(provider_key) = provider_key else {
            return false;
        };
        if let Ok(mut bindings) = self.agent_bindings.write() {
            bindings.insert(
                agent_id.to_string(),
                AgentBinding {
                    provider_key: provider_key.clone(),
                    real_model: real_model.to_string(),
                },
            );
        }
        if let Ok(mut models) = self.model_to_provider.write() {
            models.insert(real_model.to_string(), provider_key);
        }
        true
    }

    /// Look up the real model name an agent currently resolves to. Returns
    /// `None` if the agent has no binding.
    pub fn resolve_agent_model(&self, agent_id: &str) -> Option<String> {
        self.agent_bindings
            .read()
            .ok()?
            .get(agent_id)
            .map(|b| b.real_model.clone())
    }

    /// Resolve a model identifier (either an agent logical name or a real
    /// model name) to the provider instance and the real model name to use.
    fn resolve(&self, model: &str) -> Result<(Arc<dyn LlmProvider>, String)> {
        // 1. Logical agent name?
        if let Ok(bindings) = self.agent_bindings.read() {
            if let Some(binding) = bindings.get(model) {
                if let Some(p) = self.providers.get(&binding.provider_key) {
                    return Ok((Arc::clone(p), binding.real_model.clone()));
                }
            }
        }

        // 2. Real model name registered alongside an agent binding?
        if let Ok(models) = self.model_to_provider.read() {
            if let Some(provider_key) = models.get(model) {
                if let Some(p) = self.providers.get(provider_key) {
                    return Ok((Arc::clone(p), model.to_string()));
                }
            }
        }

        Err(Error::Llm(format!(
            "no provider registered for model '{model}'. \
             Register an agent or model name first."
        )))
    }

    pub async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let (provider, real_model) = self.resolve(&request.model)?;
        if real_model == request.model {
            provider.chat(request).await
        } else {
            // Rewrite model name; harness sent the logical name, provider
            // expects the real one.
            let mut req = request.clone();
            req.model = real_model;
            provider.chat(&req).await
        }
    }

    pub async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let (provider, real_model) = self.resolve(&request.model)?;
        if real_model == request.model {
            provider.chat_stream(request).await
        } else {
            let mut req = request.clone();
            req.model = real_model;
            provider.chat_stream(&req).await
        }
    }

    pub fn models(&self) -> Vec<ModelInfo> {
        self.providers.values().flat_map(|p| p.models()).collect()
    }

    /// Number of distinct provider instances currently registered.
    /// Useful for logging/diagnostics.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockProvider {
        name: String,
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
            Ok(ChatCompletionResponse {
                id: "mock-1".into(),
                object: "chat.completion".into(),
                created: 0,
                model: request.model.clone(),
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage::text("assistant", format!("Hello from {}", self.name)),
                    finish_reason: Some("stop".into()),
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    ..Default::default()
                }),
            })
        }

        fn models(&self) -> Vec<ModelInfo> {
            vec![ModelInfo {
                id: format!("{}-model", self.name),
                object: "model".into(),
                owned_by: self.name.clone(),
            }]
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    #[tokio::test]
    async fn test_router_resolves_logical_agent_name() {
        let mut router = LlmRouter::new();
        router.register_provider(
            "openai|key-a|",
            Arc::new(MockProvider {
                name: "openai-a".into(),
            }),
        );
        router.bind_agent("atlas", "openai|key-a|", "gpt-4o");

        // Harness sent the agent's logical name as the model.
        let req = ChatCompletionRequest {
            model: "atlas".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            ..Default::default()
        };
        let resp = router.chat(&req).await.unwrap();
        assert!(resp.choices[0].message.content.contains("openai-a"));
        // The provider saw the *real* model name, not the logical one.
        assert_eq!(resp.model, "gpt-4o");
    }

    #[tokio::test]
    async fn test_router_resolves_real_model_name() {
        let mut router = LlmRouter::new();
        router.register_provider(
            "openai|key-a|",
            Arc::new(MockProvider {
                name: "openai-a".into(),
            }),
        );
        router.bind_agent("atlas", "openai|key-a|", "gpt-4o");

        // Direct call with the real model name (e.g. /v1/messages passing claude-...)
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            ..Default::default()
        };
        let resp = router.chat(&req).await.unwrap();
        assert!(resp.choices[0].message.content.contains("openai-a"));
        assert_eq!(resp.model, "gpt-4o");
    }

    #[tokio::test]
    async fn test_router_two_agents_share_one_provider_instance() {
        // Both agents declare provider=openai with the same key+url.
        // Should result in one provider instance.
        let config = Config {
            agents: vec![
                crate::config::AgentConfig {
                    name: "atlas".into(),
                    llm: Some(crate::config::AgentLlmConfig {
                        provider: Some("openai".into()),
                        model: Some("gpt-4o".into()),
                        api_key: Some("k1".into()),
                        base_url: None,
                        model_path: None,
                    }),
                    ..Default::default()
                },
                crate::config::AgentConfig {
                    name: "eri".into(),
                    llm: Some(crate::config::AgentLlmConfig {
                        provider: Some("openai".into()),
                        model: Some("gpt-4o-mini".into()),
                        api_key: Some("k1".into()),
                        base_url: None,
                        model_path: None,
                    }),
                    ..Default::default()
                },
                crate::config::AgentConfig {
                    name: "other".into(),
                    llm: Some(crate::config::AgentLlmConfig {
                        provider: Some("openai".into()),
                        model: Some("gpt-4o".into()),
                        api_key: Some("k2".into()), // different key
                        base_url: None,
                        model_path: None,
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let router = LlmRouter::build_from_config(&config);
        // Two unique (provider, key, base_url) tuples → 2 provider instances.
        assert_eq!(router.provider_count(), 2);
        assert_eq!(
            router.resolve_agent_model("atlas").as_deref(),
            Some("gpt-4o")
        );
        assert_eq!(
            router.resolve_agent_model("eri").as_deref(),
            Some("gpt-4o-mini")
        );
    }

    #[tokio::test]
    async fn test_router_unknown_model_errors() {
        let router = LlmRouter::new();
        let req = ChatCompletionRequest {
            model: "nonexistent".into(),
            messages: vec![],
            ..Default::default()
        };
        assert!(router.chat(&req).await.is_err());
    }

    #[test]
    fn test_router_models_aggregates() {
        let mut router = LlmRouter::new();
        router.register_provider("a", Arc::new(MockProvider { name: "a".into() }));
        router.register_provider("b", Arc::new(MockProvider { name: "b".into() }));

        let models = router.models();
        assert_eq!(models.len(), 2);
    }

    #[tokio::test]
    async fn test_set_agent_model_repoints_to_cheaper_model() {
        // Budget degradation scenario: re-point an agent at a cheaper model
        // without rebuilding the router.
        let mut router = LlmRouter::new();
        router.register_provider(
            "openai|key|",
            Arc::new(MockProvider {
                name: "openai".into(),
            }),
        );
        router.bind_agent("atlas", "openai|key|", "gpt-4o");

        assert!(router.set_agent_model("atlas", "gpt-4o-mini"));
        assert_eq!(
            router.resolve_agent_model("atlas").as_deref(),
            Some("gpt-4o-mini")
        );

        // set_agent_model on an unknown agent returns false (no binding).
        assert!(!router.set_agent_model("ghost", "gpt-4o-mini"));
    }
}
