//! Embedded llama.cpp provider using the `llama-cpp-2` crate.
//!
//! Runs GGUF models directly in-process — no Ollama or external server needed.
//! Models can be loaded from a local path or downloaded from HuggingFace.

#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use super::router::{
    ChatChoice, ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ChatStream,
    LlmProvider, ModelInfo, Usage,
};
use crate::error::{Error, Result};

/// Default HuggingFace repo for Qwen 3.5 GGUF models.
pub const DEFAULT_GGUF_REPO: &str = "unsloth/Qwen3.5-0.8B-GGUF";

/// Default GGUF filename (smallest viable model).
pub const DEFAULT_GGUF_FILE: &str = "Qwen3.5-0.8B-UD-Q4_K_XL.gguf";

/// Download progress tracking for the frontend.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct DownloadProgress {
    pub status: DownloadStatus,
    pub filename: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub enum DownloadStatus {
    #[default]
    Idle,
    Downloading,
    Complete,
    Error,
}

/// Progress reporter that writes into shared state for the frontend to poll.
struct SharedProgress {
    state: Arc<std::sync::RwLock<DownloadProgress>>,
}

impl hf_hub::api::Progress for SharedProgress {
    fn init(&mut self, size: usize, filename: &str) {
        if let Ok(mut s) = self.state.write() {
            s.total_bytes = size as u64;
            s.filename = filename.to_string();
            s.status = DownloadStatus::Downloading;
            s.downloaded_bytes = 0;
        }
    }
    fn update(&mut self, size: usize) {
        if let Ok(mut s) = self.state.write() {
            s.downloaded_bytes += size as u64;
        }
    }
    fn finish(&mut self) {
        if let Ok(mut s) = self.state.write() {
            s.status = DownloadStatus::Complete;
        }
    }
}

/// Download a GGUF model from HuggingFace.
///
/// Uses the `hf-hub` crate which caches downloads in `~/.cache/huggingface/hub/`.
/// Subsequent calls for the same model return the cached path immediately.
pub fn download_gguf(repo_id: &str, filename: &str) -> Result<PathBuf> {
    use hf_hub::api::sync::ApiBuilder;

    tracing::info!(repo = repo_id, file = filename, "downloading GGUF model");

    let api = ApiBuilder::new()
        .with_progress(true)
        .build()
        .map_err(|e| Error::Llm(format!("HuggingFace API init failed: {e}")))?;

    let path = api
        .model(repo_id.to_string())
        .get(filename)
        .map_err(|e| Error::Llm(format!("failed to download {repo_id}/{filename}: {e}")))?;

    tracing::info!(path = %path.display(), "GGUF model ready");
    Ok(path)
}

/// Check if a GGUF model is already cached locally.
pub fn is_gguf_cached(repo_id: &str, filename: &str) -> Option<PathBuf> {
    let cache = hf_hub::Cache::from_env();
    cache.model(repo_id.to_string()).get(filename)
}

/// Download a GGUF model with progress tracking for the frontend.
pub fn download_gguf_with_progress(
    repo_id: &str,
    filename: &str,
    progress_state: Arc<std::sync::RwLock<DownloadProgress>>,
) -> Result<PathBuf> {
    use hf_hub::api::sync::ApiBuilder;

    tracing::info!(repo = repo_id, file = filename, "downloading GGUF model");

    let api = ApiBuilder::new()
        .with_progress(false)
        .build()
        .map_err(|e| Error::Llm(format!("HuggingFace API init failed: {e}")))?;

    let progress = SharedProgress {
        state: progress_state,
    };

    let path = api
        .model(repo_id.to_string())
        .download_with_progress(filename, progress)
        .map_err(|e| Error::Llm(format!("failed to download {repo_id}/{filename}: {e}")))?;

    tracing::info!(path = %path.display(), "GGUF model ready");
    Ok(path)
}

/// Default context length for inference.
const DEFAULT_CONTEXT_LENGTH: u32 = 32_768;

/// Embedded llama.cpp LLM provider.
///
/// Loads a GGUF model in-process and runs inference directly using the llama.cpp
/// C library (via safe Rust bindings). No external server required.
///
/// A fresh LlamaContext is created per inference call on the calling thread
/// to satisfy Metal's thread affinity requirements. The model is loaded once
/// and shared (read-only after loading).
pub struct LlamaCppProvider {
    backend: Arc<LlamaBackend>,
    model: Arc<LlamaModel>,
    model_name: String,
    context_length: u32,
}

// SAFETY: LlamaBackend and LlamaModel hold raw pointers to thread-safe C structs.
// The llama.cpp library supports concurrent reads on the model from multiple threads.
// Each inference call creates its own LlamaContext on the calling thread.
unsafe impl Send for LlamaCppProvider {}
unsafe impl Sync for LlamaCppProvider {}

impl LlamaCppProvider {
    /// Load a GGUF model from a local file path.
    pub fn from_path(model_path: impl AsRef<Path>, model_name: String) -> Result<Self> {
        let path = model_path.as_ref();
        if !path.exists() {
            return Err(Error::Llm(format!(
                "model file not found: {}",
                path.display()
            )));
        }

        tracing::info!(path = %path.display(), "loading GGUF model");

        let backend = Arc::new(
            LlamaBackend::init()
                .map_err(|e| Error::Llm(format!("llama backend init failed: {e}")))?,
        );

        let params = {
            let p = LlamaModelParams::default();
            #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
            let p = p.with_n_gpu_layers(0);
            p
        };
        let model = Arc::new(
            LlamaModel::load_from_file(&backend, path, &params)
                .map_err(|e| Error::Llm(format!("failed to load model: {e}")))?,
        );

        let context_length = DEFAULT_CONTEXT_LENGTH;
        tracing::info!(model = model_name, n_ctx = context_length, "GGUF model loaded");

        Ok(Self {
            backend,
            model,
            model_name,
            context_length,
        })
    }

    /// Download a GGUF model from HuggingFace and load it.
    pub fn from_huggingface(repo_id: &str, filename: &str) -> Result<Self> {
        let path = download_gguf(repo_id, filename)?;
        let model_name = filename.replace(".gguf", "");
        Self::from_path(path, model_name)
    }

    /// Load the default model (Qwen 3.5 0.8B Q4_K_XL).
    pub fn default_model() -> Result<Self> {
        Self::from_huggingface(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE)
    }

    /// Set the context length (default: 4096).
    pub fn with_context_length(mut self, n: u32) -> Self {
        self.context_length = n;
        self
    }

    /// Format chat messages into a prompt string using the model's built-in chat template.
    ///
    /// Falls back to manual ChatML formatting if the model doesn't have a template.
    fn format_prompt(&self, messages: &[ChatMessage]) -> Result<String> {
        let chat_messages: Vec<LlamaChatMessage> = messages
            .iter()
            .map(|m| {
                LlamaChatMessage::new(m.role.clone(), m.content.clone())
                    .map_err(|e| Error::Llm(format!("invalid chat message: {e}")))
            })
            .collect::<Result<Vec<_>>>()?;

        match self.model.chat_template(None) {
            Ok(tmpl) => self
                .model
                .apply_chat_template(&tmpl, &chat_messages, true)
                .map_err(|e| Error::Llm(format!("chat template failed: {e}"))),
            Err(_) => {
                // Fallback: manual ChatML formatting
                let mut prompt = String::new();
                for msg in messages {
                    prompt.push_str("<|im_start|>");
                    prompt.push_str(&msg.role);
                    prompt.push('\n');
                    prompt.push_str(&msg.content);
                    prompt.push_str("<|im_end|>\n");
                }
                prompt.push_str("<|im_start|>assistant\n");
                Ok(prompt)
            }
        }
    }

    /// Run synchronous inference on the model.
    ///
    /// Creates a fresh context on the calling thread to satisfy Metal's
    /// thread affinity requirements.
    fn generate(&self, prompt: &str, max_tokens: i32, temperature: f32) -> Result<(String, Usage)> {
        let ctx_params =
            LlamaContextParams::default().with_n_ctx(NonZeroU32::new(self.context_length));
        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| Error::Llm(format!("context creation failed: {e}")))?;

        // Tokenize the prompt (no BOS — Qwen models don't use a BOS token)
        let tokens_list = self
            .model
            .str_to_token(prompt, AddBos::Never)
            .map_err(|e| Error::Llm(format!("tokenization failed: {e}")))?;

        let prompt_tokens = tokens_list.len() as i64;
        let n_ctx = ctx.n_ctx() as i32;

        // Cap max_tokens to fit within the context window
        let max_tokens = max_tokens.min(n_ctx - tokens_list.len() as i32);
        if max_tokens <= 0 {
            return Err(Error::Llm(format!(
                "prompt ({prompt_tokens} tokens) fills the entire context ({n_ctx})"
            )));
        }
        let n_len = tokens_list.len() as i32 + max_tokens;

        // Feed prompt tokens into a batch (size must fit the prompt)
        let batch_size = tokens_list.len().max(512);
        let mut batch = LlamaBatch::new(batch_size, 1);
        let last_index = (tokens_list.len() - 1) as i32;
        for (i, token) in (0_i32..).zip(tokens_list.into_iter()) {
            batch
                .add(token, i, &[0], i == last_index)
                .map_err(|e| Error::Llm(format!("batch add failed: {e}")))?;
        }

        // Decode the prompt
        ctx.decode(&mut batch)
            .map_err(|e| Error::Llm(format!("prompt decode failed: {e}")))?;

        // Sampling: repetition penalty to avoid degenerate loops, then top-k + temperature.
        let mut sampler = if temperature > 0.0 {
            LlamaSampler::chain_simple([
                LlamaSampler::penalties(64, 1.0, 0.0, 0.0),
                LlamaSampler::top_k(40),
                LlamaSampler::min_p(0.05, 1),
                LlamaSampler::temp(temperature),
                LlamaSampler::dist(1234),
            ])
        } else {
            LlamaSampler::chain_simple([
                LlamaSampler::penalties(64, 1.0, 0.0, 0.0),
                LlamaSampler::greedy(),
            ])
        };

        // Generation loop
        let mut n_cur = batch.n_tokens();
        let mut output = String::new();
        let mut completion_tokens = 0i64;
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        while n_cur <= n_len {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);

            // End of generation?
            if self.model.is_eog_token(token) {
                break;
            }

            let piece = self
                .model
                .token_to_piece(token, &mut decoder, true, None)
                .map_err(|e| Error::Llm(format!("token decode failed: {e}")))?;
            output.push_str(&piece);
            completion_tokens += 1;

            // Stop on ChatML end token in output
            if output.ends_with("<|im_end|>") {
                output.truncate(output.len() - "<|im_end|>".len());
                break;
            }

            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| Error::Llm(format!("batch add failed: {e}")))?;
            n_cur += 1;

            ctx.decode(&mut batch)
                .map_err(|e| Error::Llm(format!("decode failed: {e}")))?;
        }

        Ok((
            output.trim().to_string(),
            Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        ))
    }
}

#[async_trait::async_trait]
impl LlmProvider for LlamaCppProvider {
    async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let prompt = self.format_prompt(&request.messages)?;
        let max_tokens = request.max_tokens.unwrap_or(256) as i32;
        let temperature = request.temperature.unwrap_or(0.8) as f32;

        let (content, usage) = self.generate(&prompt, max_tokens, temperature)?;

        Ok(ChatCompletionResponse {
            id: format!("llamacpp-{}", uuid::Uuid::new_v4()),
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp(),
            model: self.model_name.clone(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content,
                },
                finish_reason: Some("stop".into()),
            }],
            usage: Some(usage),
        })
    }

    async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let prompt = self.format_prompt(&request.messages)?;
        let max_tokens = request.max_tokens.unwrap_or(256) as i32;
        let temperature = request.temperature.unwrap_or(0.8) as f32;

        let model = self.model.clone();
        let backend = self.backend.clone();
        let model_name = self.model_name.clone();
        let context_length = self.context_length;
        let id = format!("llamacpp-{}", uuid::Uuid::new_v4());
        let created = chrono::Utc::now().timestamp();

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<super::router::ChatCompletionChunk>>(32);

        let id_clone = id.clone();
        let model_name_clone = model_name.clone();
        tokio::task::spawn_blocking(move || {
            let send_chunk = |content: String, finish: Option<String>| {
                let chunk = super::router::ChatCompletionChunk {
                    id: id_clone.clone(),
                    object: "chat.completion.chunk".into(),
                    created,
                    model: model_name_clone.clone(),
                    choices: vec![super::router::ChunkChoice {
                        index: 0,
                        delta: super::router::ChunkDelta {
                            role: None,
                            content: if content.is_empty() { None } else { Some(content) },
                        },
                        finish_reason: finish,
                    }],
                };
                let _ = tx.blocking_send(Ok(chunk));
            };

            // Create context
            let ctx_params =
                LlamaContextParams::default().with_n_ctx(NonZeroU32::new(context_length));
            let mut ctx = match model.new_context(&backend, ctx_params) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.blocking_send(Err(Error::Llm(format!("context failed: {e}"))));
                    return;
                }
            };

            let tokens_list = match model.str_to_token(&prompt, AddBos::Never) {
                Ok(t) => t,
                Err(e) => {
                    let _ = tx.blocking_send(Err(Error::Llm(format!("tokenize failed: {e}"))));
                    return;
                }
            };

            let n_ctx = ctx.n_ctx() as i32;
            let max_tokens = max_tokens.min(n_ctx - tokens_list.len() as i32);
            if max_tokens <= 0 {
                let _ = tx.blocking_send(Err(Error::Llm("prompt fills entire context".into())));
                return;
            }
            let n_len = tokens_list.len() as i32 + max_tokens;

            let batch_size = tokens_list.len().max(512);
            let mut batch = LlamaBatch::new(batch_size, 1);
            let last_index = (tokens_list.len() - 1) as i32;
            for (i, token) in (0_i32..).zip(tokens_list.into_iter()) {
                let _ = batch.add(token, i, &[0], i == last_index);
            }
            if ctx.decode(&mut batch).is_err() {
                let _ = tx.blocking_send(Err(Error::Llm("prompt decode failed".into())));
                return;
            }

            let mut sampler = if temperature > 0.0 {
                LlamaSampler::chain_simple([
                    LlamaSampler::penalties(64, 1.0, 0.0, 0.0),
                    LlamaSampler::top_k(40),
                    LlamaSampler::min_p(0.05, 1),
                    LlamaSampler::temp(temperature),
                    LlamaSampler::dist(1234),
                ])
            } else {
                LlamaSampler::chain_simple([
                    LlamaSampler::penalties(64, 1.0, 0.0, 0.0),
                    LlamaSampler::greedy(),
                ])
            };

            let mut n_cur = batch.n_tokens();
            let mut decoder = encoding_rs::UTF_8.new_decoder();
            let mut full_output = String::new();

            while n_cur <= n_len {
                let token = sampler.sample(&ctx, batch.n_tokens() - 1);
                sampler.accept(token);

                if model.is_eog_token(token) {
                    break;
                }

                let piece = match model.token_to_piece(token, &mut decoder, true, None) {
                    Ok(p) => p,
                    Err(_) => break,
                };
                full_output.push_str(&piece);

                // Send token immediately
                send_chunk(piece, None);

                if full_output.ends_with("<|im_end|>") {
                    break;
                }

                batch.clear();
                let _ = batch.add(token, n_cur, &[0], true);
                n_cur += 1;
                if ctx.decode(&mut batch).is_err() {
                    break;
                }
            }

            // Send final chunk with finish_reason
            send_chunk(String::new(), Some("stop".into()));
        });

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        Ok(Box::pin(stream))
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: self.model_name.clone(),
            object: "model".into(),
            owned_by: "local-llamacpp".into(),
        }]
    }

    fn name(&self) -> &str {
        "llamacpp"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_gguf_url_components() {
        // Verify the default constants are correct
        assert_eq!(DEFAULT_GGUF_REPO, "unsloth/Qwen3.5-0.8B-GGUF");
        assert_eq!(DEFAULT_GGUF_FILE, "Qwen3.5-0.8B-UD-Q4_K_XL.gguf");
    }

    /// Minimal inference test — mirrors the reference usage.rs exactly.
    #[ignore = "downloads ~600MB model from HuggingFace"]
    #[test]
    fn test_llamacpp_minimal() {
        use std::io::Write;

        let path = download_gguf(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE)
            .expect("download failed");

        let backend = LlamaBackend::init().unwrap();
        // Force CPU-only (ngl=0) — Metal on x86_64 Mac produces incorrect results
        // with some quantization formats.
        let params = LlamaModelParams::default().with_n_gpu_layers(0);
        let model = LlamaModel::load_from_file(&backend, path, &params)
            .expect("unable to load model");

        // Use model's chat template
        let tmpl = model.chat_template(None).expect("no chat template");
        let msgs = vec![
            LlamaChatMessage::new("user".into(), "What is 2+2? Answer briefly.".into()).unwrap(),
        ];
        let prompt = model.apply_chat_template(&tmpl, &msgs, true)
            .expect("template failed");
        eprintln!("Prompt: {prompt:?}");

        let ctx_params = LlamaContextParams::default();
        let mut ctx = model.new_context(&backend, ctx_params)
            .expect("unable to create context");

        let tokens_list = model.str_to_token(&prompt, AddBos::Always)
            .expect("tokenization failed");
        eprintln!("Token count: {}", tokens_list.len());

        let mut batch = LlamaBatch::new(512, 1);
        let last_index = tokens_list.len() as i32 - 1;
        for (i, token) in (0_i32..).zip(tokens_list.into_iter()) {
            batch.add(token, i, &[0], i == last_index).unwrap();
        }
        ctx.decode(&mut batch).expect("decode failed");

        let mut n_cur = batch.n_tokens();
        let n_len = n_cur + 64;
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut sampler = LlamaSampler::greedy();
        let mut output = String::new();

        while n_cur <= n_len {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);

            if model.is_eog_token(token) {
                eprintln!("\n[EOG]");
                break;
            }

            let piece = model.token_to_piece(token, &mut decoder, true, None).unwrap();
            eprint!("{piece}");
            std::io::stderr().flush().ok();
            output.push_str(&piece);

            batch.clear();
            batch.add(token, n_cur, &[0], true).unwrap();
            n_cur += 1;
            ctx.decode(&mut batch).expect("decode failed");
        }

        eprintln!("\n\nFull output: {output:?}");
        assert!(!output.is_empty(), "empty output");
        let unique: std::collections::HashSet<char> = output.chars().collect();
        assert!(unique.len() > 3, "degenerate output: {output}");
    }

    /// Full integration test using the provider API.
    #[ignore = "downloads ~600MB model from HuggingFace"]
    #[test]
    fn test_llamacpp_qwen_inference() {
        let provider = LlamaCppProvider::from_huggingface(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE)
            .expect("failed to load Qwen 3.5 0.8B");

        let prompt = provider
            .format_prompt(&[ChatMessage {
                role: "user".into(),
                content: "Explain what a binary tree is in 2-3 sentences.".into(),
            }])
            .expect("format_prompt failed");

        eprintln!("Prompt: {prompt:?}");

        let (output, usage) = provider
            .generate(&prompt, 128, 0.8)
            .expect("inference failed");

        eprintln!("Output: {output:?}");
        eprintln!("Usage: {usage:?}");

        assert!(!output.is_empty(), "model produced empty output");
        assert!(usage.prompt_tokens > 0);
        assert!(usage.completion_tokens > 0);

        let unique_chars: std::collections::HashSet<char> = output.chars().collect();
        assert!(
            unique_chars.len() > 3,
            "output looks degenerate (too few unique chars): {output}"
        );
    }

    /// Test the async LlmProvider interface.
    #[ignore = "downloads ~600MB model from HuggingFace"]
    #[tokio::test]
    async fn test_llamacpp_provider_chat() {
        let provider = LlamaCppProvider::from_huggingface(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE)
            .expect("failed to load model");

        let request = ChatCompletionRequest {
            model: "qwen3.5:0.8b".into(),
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "Say hello in exactly one word.".into(),
            }],
            temperature: Some(0.0),
            max_tokens: Some(16),
            stream: None,
            top_p: None,
            stop: None,
        };

        let response = provider.chat(&request).await.expect("chat failed");

        assert_eq!(response.object, "chat.completion");
        assert!(!response.choices.is_empty());
        assert_eq!(response.choices[0].message.role, "assistant");
        assert!(
            !response.choices[0].message.content.is_empty(),
            "assistant response should not be empty"
        );
        assert!(response.usage.is_some());

        eprintln!("Response: {:?}", response.choices[0].message.content);
    }

}
