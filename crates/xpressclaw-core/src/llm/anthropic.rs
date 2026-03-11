use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::{Error, Result};

use super::router::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, LlmProvider, ModelInfo,
    Usage,
};

/// Anthropic API provider.
///
/// Translates between OpenAI-format requests and the Anthropic Messages API.
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }
}

// -- Anthropic API types --

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    max_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    id: String,
    model: String,
    content: Vec<AnthropicContent>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicContent {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    content_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: i64,
    output_tokens: i64,
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        // Extract system message
        let system = request
            .messages
            .iter()
            .find(|m| m.role == "system")
            .map(|m| m.content.clone());

        // Convert messages (skip system)
        let messages: Vec<AnthropicMessage> = request
            .messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| AnthropicMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            })
            .collect();

        let anthropic_req = AnthropicRequest {
            model: request.model.clone(),
            messages,
            max_tokens: request.max_tokens.unwrap_or(4096),
            temperature: request.temperature,
            system,
            top_p: request.top_p,
            stop_sequences: request.stop.clone(),
        };

        debug!(model = request.model, "sending request to Anthropic API");

        let resp = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&anthropic_req)
            .send()
            .await
            .map_err(|e| Error::Llm(format!("Anthropic request failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Llm(format!("Anthropic API error {status}: {body}")));
        }

        let anthropic_resp: AnthropicResponse = resp
            .json()
            .await
            .map_err(|e| Error::Llm(format!("Failed to parse Anthropic response: {e}")))?;

        // Convert to OpenAI format
        let content = anthropic_resp
            .content
            .iter()
            .filter_map(|c| c.text.as_ref())
            .cloned()
            .collect::<Vec<_>>()
            .join("");

        let finish_reason = anthropic_resp.stop_reason.map(|r| match r.as_str() {
            "end_turn" => "stop".to_string(),
            "max_tokens" => "length".to_string(),
            other => other.to_string(),
        });

        Ok(ChatCompletionResponse {
            id: anthropic_resp.id,
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp(),
            model: anthropic_resp.model,
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content,
                },
                finish_reason,
            }],
            usage: Some(Usage {
                prompt_tokens: anthropic_resp.usage.input_tokens,
                completion_tokens: anthropic_resp.usage.output_tokens,
                total_tokens: anthropic_resp.usage.input_tokens
                    + anthropic_resp.usage.output_tokens,
            }),
        })
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "claude-opus-4-5-20251101".into(),
                object: "model".into(),
                owned_by: "anthropic".into(),
            },
            ModelInfo {
                id: "claude-sonnet-4-5-20251022".into(),
                object: "model".into(),
                owned_by: "anthropic".into(),
            },
            ModelInfo {
                id: "claude-haiku-4-5-20251022".into(),
                object: "model".into(),
                owned_by: "anthropic".into(),
            },
        ]
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
