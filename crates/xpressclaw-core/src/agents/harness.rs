use reqwest::Client;
use tracing::debug;

use crate::error::{Error, Result};
use crate::llm::router::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ChatStream,
    ChunkChoice, ChunkDelta,
};

/// Client for communicating with agent harness containers.
///
/// Harness containers expose an OpenAI-compatible `/v1/chat/completions` endpoint.
/// The server sends task prompts to the harness, which runs the agent loop internally.
pub struct HarnessClient {
    client: Client,
    base_url: String,
}

impl HarnessClient {
    /// Create a client pointing to a harness container.
    pub fn new(host_port: u16) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            base_url: format!("http://127.0.0.1:{host_port}"),
        }
    }

    pub fn from_url(base_url: String) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            base_url,
        }
    }

    /// Send a task to the harness and get the response.
    pub async fn send_task(
        &self,
        system_prompt: &str,
        task_prompt: &str,
        model: &str,
    ) -> Result<ChatCompletionResponse> {
        let request = ChatCompletionRequest {
            model: model.to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: system_prompt.to_string(),
                    ..Default::default()
                },
                ChatMessage {
                    role: "user".into(),
                    content: task_prompt.to_string(),
                    ..Default::default()
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(4096),
            stream: Some(false),
            top_p: None,
            stop: None,
            ..Default::default()
        };

        self.chat(&request).await
    }

    /// Send a raw chat completion request (non-streaming).
    pub async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        debug!(url, model = request.model, "sending request to harness");

        let resp = self
            .client
            .post(&url)
            .json(request)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| Error::Agent(format!("harness request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Agent(format!("harness error {status}: {body}")));
        }

        resp.json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse harness response: {e}")))
    }

    /// Send a streaming chat completion request. Returns a stream of chunks.
    pub async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let mut stream_req = request.clone();
        stream_req.stream = Some(true);

        debug!(
            url,
            model = request.model,
            "sending streaming request to harness"
        );

        let resp = self
            .client
            .post(&url)
            .json(&stream_req)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("harness stream request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "harness stream error {status}: {body}"
            )));
        }

        let byte_stream = resp.bytes_stream();
        let stream = parse_sse_stream(byte_stream);
        Ok(Box::pin(stream))
    }

    /// Send a message to the agent's persistent session.
    /// Returns a stream of SSE chunks (may be buffered by the SDK).
    pub async fn send_session_message(
        &self,
        message: &str,
        conversation_id: &str,
        sender_name: &str,
        sender_type: &str,
        system_prompt: &str,
    ) -> Result<ChatStream> {
        let url = format!("{}/v1/session/send", self.base_url);

        let body = serde_json::json!({
            "message": message,
            "conversation_id": conversation_id,
            "sender_name": sender_name,
            "sender_type": sender_type,
            "system_prompt": system_prompt,
            "stream": true,
        });

        debug!(url, conversation_id, "sending session message to harness");

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Agent(format!("harness session request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "harness session error {status}: {body}"
            )));
        }

        let byte_stream = resp.bytes_stream();
        let stream = parse_sse_stream(byte_stream);
        Ok(Box::pin(stream))
    }

    /// Send a message to the agent's session (non-streaming).
    pub async fn send_session_message_sync(
        &self,
        message: &str,
        conversation_id: &str,
        sender_name: &str,
        sender_type: &str,
        system_prompt: &str,
    ) -> Result<String> {
        let url = format!("{}/v1/session/send", self.base_url);

        let body = serde_json::json!({
            "message": message,
            "conversation_id": conversation_id,
            "sender_name": sender_name,
            "sender_type": sender_type,
            "system_prompt": system_prompt,
            "stream": false,
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| Error::Agent(format!("harness session request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Agent(format!(
                "harness session error {status}: {body}"
            )));
        }

        let data: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| Error::Agent(format!("failed to parse session response: {e}")))?;

        Ok(data["content"].as_str().unwrap_or("").to_string())
    }

    /// Check if the harness is healthy.
    pub async fn health_check(&self) -> bool {
        let url = format!("{}/health", self.base_url);
        match self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Signal the harness to cancel at its next tool call.
    pub async fn cancel(&self) -> Result<()> {
        let url = format!("{}/v1/cancel", self.base_url);
        self.client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| Error::Llm(format!("cancel failed: {e}")))?;
        Ok(())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Parse an SSE byte stream into ChatCompletionChunk items.
fn parse_sse_stream(
    byte_stream: impl futures_util::Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>>
        + Send
        + 'static,
) -> impl futures_util::Stream<Item = Result<ChatCompletionChunk>> + Send {
    async_stream::stream! {
        use futures_util::StreamExt;

        let mut buffer = String::new();

        futures_util::pin_mut!(byte_stream);
        while let Some(chunk_result) = byte_stream.next().await {
            let bytes = match chunk_result {
                Ok(b) => b,
                Err(e) => {
                    yield Err(Error::Agent(format!("SSE read error: {e}")));
                    return;
                }
            };

            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Process complete SSE messages (separated by \n\n)
            while let Some(pos) = buffer.find("\n\n") {
                let message = buffer[..pos].to_string();
                buffer = buffer[pos + 2..].to_string();

                // Extract data line
                let data = message
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .unwrap_or("")
                    .trim();

                if data.is_empty() || data == "[DONE]" {
                    continue;
                }

                match serde_json::from_str::<ChatCompletionChunk>(data) {
                    Ok(chunk) => yield Ok(chunk),
                    Err(_) => {
                        // Skip unparseable chunks (e.g., error objects)
                        // Build a synthetic error chunk if it looks like an error
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                            if let Some(err_msg) = val.get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str()) {
                                yield Ok(ChatCompletionChunk {
                                    id: "error".into(),
                                    object: "chat.completion.chunk".into(),
                                    created: 0,
                                    model: String::new(),
                                    choices: vec![ChunkChoice {
                                        index: 0,
                                        delta: ChunkDelta {
                                            role: Some("assistant".into()),
                                            content: Some(format!("Error: {err_msg}")),
                                            ..Default::default()
                                        },
                                        finish_reason: Some("stop".into()),
                                    }],
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}
