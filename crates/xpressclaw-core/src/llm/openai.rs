use reqwest::Client;
use tracing::debug;

use crate::error::{Error, Result};

use super::router::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ChatStream, LlmProvider,
    ModelInfo,
};

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

    /// Validate an OpenAI API key by listing models.
    pub async fn validate_key(
        api_key: &str,
        base_url: Option<&str>,
    ) -> std::result::Result<bool, String> {
        let base = base_url.unwrap_or("https://api.openai.com");
        let url = format!("{base}/v1/models");

        let client = Client::new();
        let resp = client
            .get(&url)
            .bearer_auth(api_key)
            .send()
            .await
            .map_err(|e| format!("Request failed: {e}"))?;

        Ok(resp.status().is_success())
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

    async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        // Force stream=true
        let mut stream_req = request.clone();
        stream_req.stream = Some(true);

        let mut req = self.client.post(&url).json(&stream_req);

        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }

        debug!(
            model = request.model,
            "sending streaming request to OpenAI-compatible API"
        );

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Llm(format!("OpenAI stream request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Llm(format!("OpenAI API error {status}: {body}")));
        }

        let byte_stream = resp.bytes_stream();

        Ok(Box::pin(parse_sse_stream(byte_stream)))
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

/// Parse an SSE byte stream into ChatCompletionChunk items.
pub fn parse_sse_stream(
    byte_stream: impl futures_util::Stream<Item = std::result::Result<impl AsRef<[u8]>, reqwest::Error>>
        + Send
        + 'static,
) -> impl futures_util::Stream<Item = Result<ChatCompletionChunk>> {
    use futures_util::StreamExt;

    let mut buffer = String::new();

    byte_stream
        .map(move |chunk| match chunk {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(bytes.as_ref());
                buffer.push_str(&text);

                let mut chunks = Vec::new();
                while let Some(pos) = buffer.find("\n\n") {
                    let line = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    let data = line.strip_prefix("data: ").unwrap_or(&line);
                    if data == "[DONE]" || data.is_empty() {
                        continue;
                    }

                    if let Ok(chunk) = serde_json::from_str::<ChatCompletionChunk>(data) {
                        chunks.push(Ok(chunk));
                    }
                }
                futures_util::stream::iter(chunks)
            }
            Err(e) => {
                futures_util::stream::iter(vec![Err(Error::Llm(format!("Stream error: {e}")))])
            }
        })
        .flatten()
}
