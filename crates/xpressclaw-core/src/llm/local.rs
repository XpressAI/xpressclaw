use super::router::{ChatCompletionRequest, ChatCompletionResponse, LlmProvider, ModelInfo};
use crate::error::{Error, Result};

/// Local LLM provider that proxies to an OpenAI-compatible server.
///
/// Works with Ollama, vLLM, llama.cpp server, or any other local server
/// that exposes an OpenAI-compatible `/v1/chat/completions` endpoint.
///
/// The xpressclaw server exposes its own `/v1/chat/completions` endpoint
/// and passes `OPENAI_BASE_URL` into agent containers so they can call
/// back to it. This provider handles routing to the local LLM backend.
pub struct LocalProvider {
    base_url: String,
    model_name: String,
    client: reqwest::Client,
}

impl LocalProvider {
    /// Create a provider that connects to a local OpenAI-compatible server.
    pub fn new(base_url: String, model_name: String) -> Self {
        Self {
            base_url,
            model_name,
            client: reqwest::Client::new(),
        }
    }

    /// Connect to Ollama's OpenAI-compatible endpoint.
    pub fn ollama(model_name: String) -> Self {
        Self::new("http://localhost:11434".to_string(), model_name)
    }
}

#[async_trait::async_trait]
impl LlmProvider for LocalProvider {
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
            return Err(Error::Llm(format!("Local LLM error {status}: {body}")));
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
