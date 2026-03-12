use reqwest::Client;
use tracing::debug;

use crate::error::{Error, Result};

use super::router::{ChatCompletionRequest, ChatCompletionResponse, LlmProvider, ModelInfo};

/// OpenAI-compatible provider.
///
/// Works with OpenAI, OpenRouter, vLLM, Ollama, and any OpenAI-compatible API.
pub struct OpenAiProvider {
    client: Client,
    api_key: Option<String>,
    base_url: String,
    available_models: Vec<String>,
}

impl OpenAiProvider {
    pub fn new(api_key: Option<String>, base_url: Option<String>) -> Self {
        let base_url = base_url.unwrap_or_else(|| "https://api.openai.com".to_string());
        Self {
            client: Client::new(),
            api_key,
            base_url,
            available_models: vec![
                "gpt-5.2".into(),
                "gpt-5-mini".into(),
                "gpt-4o".into(),
                "gpt-4o-mini".into(),
            ],
        }
    }

    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.available_models = models;
        self
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let mut req = self.client.post(&url).json(request);

        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }

        debug!(
            model = request.model,
            "sending request to OpenAI-compatible API"
        );

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Llm(format!("OpenAI request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Llm(format!("OpenAI API error {status}: {body}")));
        }

        let completion: ChatCompletionResponse = resp
            .json()
            .await
            .map_err(|e| Error::Llm(format!("Failed to parse OpenAI response: {e}")))?;

        Ok(completion)
    }

    fn models(&self) -> Vec<ModelInfo> {
        self.available_models
            .iter()
            .map(|m| ModelInfo {
                id: m.clone(),
                object: "model".into(),
                owned_by: "openai".into(),
            })
            .collect()
    }

    fn name(&self) -> &str {
        "openai"
    }
}
