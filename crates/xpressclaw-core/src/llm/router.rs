use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::config::LlmConfig;
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

/// Routes LLM requests to the appropriate provider based on model name.
pub struct LlmRouter {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    model_to_provider: HashMap<String, String>,
    default_provider: String,
}

impl LlmRouter {
    pub fn new(config: &LlmConfig) -> Self {
        Self {
            providers: HashMap::new(),
            model_to_provider: HashMap::new(),
            default_provider: config.default_provider.clone(),
        }
    }

    /// Build a fully configured LLM router from config.
    ///
    /// Registers all providers based on config:
    /// - OpenAI if API key is set
    /// - Anthropic if API key is set
    /// - Local model: uses embedded llama.cpp (LlamaCppProvider) if `local_model_path`
    ///   is set, otherwise falls back to HTTP proxy (LocalProvider for Ollama/vLLM/etc.)
    pub fn build_from_config(config: &LlmConfig) -> Self {
        let mut router = Self::new(config);

        if let Some(ref key) = config.openai_api_key {
            let provider = super::openai::OpenAiProvider::new(
                Some(key.clone()),
                config.openai_base_url.clone(),
            );
            router.register_provider("openai", Arc::new(provider));
        }

        if let Some(ref key) = config.anthropic_api_key {
            let provider = super::anthropic::AnthropicProvider::new(key.clone());
            router.register_provider("anthropic", Arc::new(provider));
        }

        // "local" provider = embedded llama.cpp (GGUF model)
        if let Some(ref path) = config.local_model_path {
            #[cfg(feature = "local-llm")]
            {
                let model_name = config
                    .local_model
                    .clone()
                    .unwrap_or_else(|| "local".to_string());
                match super::llamacpp::LazyLlamaCppProvider::new(
                    std::path::PathBuf::from(path),
                    model_name,
                ) {
                    Ok(provider) => {
                        tracing::info!(path = %path, "registered embedded llama.cpp provider");
                        router.register_provider("local", Arc::new(provider));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "GGUF model not found");
                    }
                }
            }
        }

        // "ollama" provider = HTTP proxy to Ollama/vLLM/llama-server
        if let Some(ref model) = config.local_model {
            let provider = super::local::LocalProvider::from_config(
                model.clone(),
                config.local_base_url.clone(),
            );
            router.register_provider("ollama", Arc::new(provider));
        }

        router
    }

    pub fn register_provider(&mut self, name: &str, provider: Arc<dyn LlmProvider>) {
        self.providers.insert(name.to_string(), provider);
    }

    pub fn register_model(&mut self, model: &str, provider_name: &str) {
        self.model_to_provider
            .insert(model.to_string(), provider_name.to_string());
    }

    fn resolve_provider(&self, model: &str) -> Result<&Arc<dyn LlmProvider>> {
        // Check explicit model→provider mapping first
        if let Some(provider_name) = self.model_to_provider.get(model) {
            if let Some(provider) = self.providers.get(provider_name) {
                return Ok(provider);
            }
        }

        // Use the default provider for everything
        self.providers.get(&self.default_provider).ok_or_else(|| {
            Error::Llm(format!(
                "no provider registered for model '{model}' (default provider '{}')",
                self.default_provider
            ))
        })
    }

    pub async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let provider = self.resolve_provider(&request.model)?;
        provider.chat(request).await
    }

    pub async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let provider = self.resolve_provider(&request.model)?;
        provider.chat_stream(request).await
    }

    pub fn models(&self) -> Vec<ModelInfo> {
        self.providers.values().flat_map(|p| p.models()).collect()
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
    async fn test_router_uses_default_provider() {
        let config = LlmConfig {
            default_provider: "openai".into(),
            ..Default::default()
        };
        let mut router = LlmRouter::new(&config);

        router.register_provider(
            "openai",
            Arc::new(MockProvider {
                name: "openai".into(),
            }),
        );
        router.register_provider(
            "anthropic",
            Arc::new(MockProvider {
                name: "anthropic".into(),
            }),
        );

        // All models go to default provider regardless of name
        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            ..Default::default()
        };
        let resp = router.chat(&req).await.unwrap();
        assert!(resp.choices[0].message.content.contains("openai"));

        let req2 = ChatCompletionRequest {
            model: "qwen3.5-27b".into(),
            messages: vec![ChatMessage::text("user", "hi")],
            ..Default::default()
        };
        let resp2 = router.chat(&req2).await.unwrap();
        assert!(resp2.choices[0].message.content.contains("openai"));
    }

    #[tokio::test]
    async fn test_router_unknown_model_errors() {
        let config = LlmConfig::default();
        let router = LlmRouter::new(&config);

        let req = ChatCompletionRequest {
            model: "nonexistent".into(),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            stop: None,
            ..Default::default()
        };

        assert!(router.chat(&req).await.is_err());
    }

    #[test]
    fn test_router_models_aggregates() {
        let config = LlmConfig::default();
        let mut router = LlmRouter::new(&config);
        router.register_provider("a", Arc::new(MockProvider { name: "a".into() }));
        router.register_provider("b", Arc::new(MockProvider { name: "b".into() }));

        let models = router.models();
        assert_eq!(models.len(), 2);
    }
}
