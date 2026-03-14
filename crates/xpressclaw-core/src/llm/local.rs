use serde::{Deserialize, Serialize};

use super::router::{
    ChatCompletionRequest, ChatCompletionResponse, ChatStream, LlmProvider, ModelInfo,
};
use crate::error::{Error, Result};

/// Information about a running Ollama instance.
#[derive(Debug, Clone, Serialize)]
pub struct OllamaInfo {
    pub available: bool,
    pub models: Vec<OllamaModel>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    pub size: Option<u64>,
}

#[derive(Deserialize)]
struct OllamaTagsResponse {
    models: Option<Vec<OllamaTagModel>>,
}

#[derive(Deserialize)]
struct OllamaTagModel {
    name: String,
    size: Option<u64>,
}

/// Check if Ollama is running and list installed models.
pub async fn detect_ollama() -> OllamaInfo {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    match client.get("http://localhost:11434/api/tags").send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<OllamaTagsResponse>().await {
            Ok(tags) => {
                let models = tags
                    .models
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| OllamaModel {
                        name: m.name,
                        size: m.size,
                    })
                    .collect();
                OllamaInfo {
                    available: true,
                    models,
                    error: None,
                }
            }
            Err(e) => OllamaInfo {
                available: true,
                models: vec![],
                error: Some(format!("Failed to parse Ollama response: {e}")),
            },
        },
        Ok(resp) => OllamaInfo {
            available: false,
            models: vec![],
            error: Some(format!("Ollama returned status {}", resp.status())),
        },
        Err(e) => OllamaInfo {
            available: false,
            models: vec![],
            error: Some(format!("Ollama is not running: {e}")),
        },
    }
}

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

    async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let mut stream_req = request.clone();
        stream_req.stream = Some(true);

        let resp = self
            .client
            .post(&url)
            .json(&stream_req)
            .send()
            .await
            .map_err(|e| Error::Llm(format!("Local LLM stream request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Llm(format!("Local LLM error {status}: {body}")));
        }

        Ok(Box::pin(super::openai::parse_sse_stream(
            resp.bytes_stream(),
        )))
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
