use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::config::LlmConfig;
use crate::error::{Error, Result};

/// A boxed stream of chat completion chunks.
pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk>> + Send>>;

/// A chat message in OpenAI format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Request for chat completion (OpenAI-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<i64>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop: Option<Vec<String>>,
}

/// Token usage stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
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
                    },
                    finish_reason: c.finish_reason,
                })
                .collect(),
        };
        Ok(Box::pin(futures_util::stream::once(async move {
            Ok(chunk)
        })))
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

    pub fn register_provider(&mut self, name: &str, provider: Arc<dyn LlmProvider>) {
        self.providers.insert(name.to_string(), provider);
    }

    pub fn register_model(&mut self, model: &str, provider_name: &str) {
        self.model_to_provider
            .insert(model.to_string(), provider_name.to_string());
    }

    fn resolve_provider(&self, model: &str) -> Result<&Arc<dyn LlmProvider>> {
        // Check explicit model→provider mapping
        if let Some(provider_name) = self.model_to_provider.get(model) {
            if let Some(provider) = self.providers.get(provider_name) {
                return Ok(provider);
            }
        }

        // Route by model name prefix
        let provider_name = if model.starts_with("claude") {
            "anthropic"
        } else if model.starts_with("gpt") || model.starts_with("o1") || model.starts_with("o3") {
            "openai"
        } else if model == "local"
            || model.starts_with("Qwen/")
            || model.starts_with("qwen")
            || model.starts_with("llama")
            || model.starts_with("meta-llama/")
        {
            "local"
        } else {
            &self.default_provider
        };

        self.providers.get(provider_name).ok_or_else(|| {
            Error::Llm(format!(
                "no provider registered for model '{model}' (tried '{provider_name}')"
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
                    message: ChatMessage {
                        role: "assistant".into(),
                        content: format!("Hello from {}", self.name),
                    },
                    finish_reason: Some("stop".into()),
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
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
    async fn test_router_routes_by_prefix() {
        let config = LlmConfig::default();
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

        let req = ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            temperature: None,
            max_tokens: None,
            stream: None,
            top_p: None,
            stop: None,
        };

        let resp = router.chat(&req).await.unwrap();
        assert!(resp.choices[0].message.content.contains("openai"));

        let req2 = ChatCompletionRequest {
            model: "claude-sonnet-4.5".into(),
            ..req
        };
        let resp2 = router.chat(&req2).await.unwrap();
        assert!(resp2.choices[0].message.content.contains("anthropic"));
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
