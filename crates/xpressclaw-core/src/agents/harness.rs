use reqwest::Client;
use tracing::debug;

use crate::error::{Error, Result};
use crate::llm::router::{ChatCompletionRequest, ChatCompletionResponse, ChatMessage};

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
            client: Client::new(),
            base_url: format!("http://127.0.0.1:{host_port}"),
        }
    }

    pub fn from_url(base_url: String) -> Self {
        Self {
            client: Client::new(),
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
                },
                ChatMessage {
                    role: "user".into(),
                    content: task_prompt.to_string(),
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(4096),
            stream: Some(false),
            top_p: None,
            stop: None,
        };

        self.chat(&request).await
    }

    /// Send a raw chat completion request.
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

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}
