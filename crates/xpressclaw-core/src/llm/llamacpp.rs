use super::router::{LlmProvider, ModelInfo, ChatCompletionRequest, ChatCompletionResponse};
use crate::error::{Error, Result};

/// Embedded llama.cpp provider via llama-cpp-rs.
///
/// This will use llama-cpp-rs with Metal (macOS) and CUDA (Linux/Windows) backends.
/// For now, this delegates to an OpenAI-compatible local server endpoint,
/// which supports vLLM, Ollama, or llama.cpp server mode.
pub struct LlamaCppProvider {
    base_url: String,
    model_name: String,
    client: reqwest::Client,
}

impl LlamaCppProvider {
    /// Create a provider that connects to a local OpenAI-compatible server.
    pub fn new(base_url: String, model_name: String) -> Self {
        Self {
            base_url,
            model_name,
            client: reqwest::Client::new(),
        }
    }

    /// Default: connect to Ollama's OpenAI-compatible endpoint.
    pub fn ollama(model_name: String) -> Self {
        Self::new("http://localhost:11434".to_string(), model_name)
    }
}

#[async_trait::async_trait]
impl LlmProvider for LlamaCppProvider {
    async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let resp = self
            .client
            .post(&url)
            .json(request)
            .send()
            .await
            .map_err(|e| Error::Llm(format!("Local LLM request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Llm(format!(
                "Local LLM error {status}: {body}"
            )));
        }

        resp.json()
            .await
            .map_err(|e| Error::Llm(format!("Failed to parse local LLM response: {e}")))
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: self.model_name.clone(),
            object: "model".into(),
            owned_by: "local".into(),
        }]
    }

    fn name(&self) -> &str {
        "local"
    }
}
