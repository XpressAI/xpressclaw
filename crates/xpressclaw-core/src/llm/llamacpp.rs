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
/// Default context length. 128k supports tool-calling (Claude SDK sends ~18k tokens
/// of tool definitions) with ample room for conversation. KV cache at 128k is ~4GB.
const DEFAULT_CONTEXT_LENGTH: u32 = 131_072;

/// Global LlamaBackend singleton — llama.cpp only allows one backend per process.
static LLAMA_BACKEND: std::sync::OnceLock<Arc<LlamaBackend>> = std::sync::OnceLock::new();

fn get_or_init_backend() -> Result<Arc<LlamaBackend>> {
    if let Some(b) = LLAMA_BACKEND.get() {
        return Ok(b.clone());
    }
    let backend =
        LlamaBackend::init().map_err(|e| Error::Llm(format!("llama backend init failed: {e}")))?;
    let arc = Arc::new(backend);
    // Another thread might have initialized it between get() and here — that's fine.
    let _ = LLAMA_BACKEND.set(arc.clone());
    Ok(LLAMA_BACKEND.get().unwrap().clone())
}

/// Prompt with optional grammar for constrained tool-call decoding.
struct PromptWithGrammar {
    prompt: String,
    grammar: Option<String>,
    grammar_lazy: bool,
}

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

        let backend = get_or_init_backend()?;

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

        // Use the model's trained context length, capped at our default
        let n_ctx_train = model.n_ctx_train();
        let context_length = if n_ctx_train > 0 {
            (n_ctx_train as u32).min(DEFAULT_CONTEXT_LENGTH)
        } else {
            DEFAULT_CONTEXT_LENGTH
        };
        tracing::info!(
            model = model_name,
            n_ctx = context_length,
            n_ctx_train,
            "GGUF model loaded"
        );

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

    /// Format chat messages with optional tool definitions.
    ///
    /// When tools are provided, uses `apply_chat_template_with_tools_oaicompat` which
    /// renders tools into the prompt using the model's Jinja template and returns a
    /// grammar for constrained decoding of tool calls.
    fn format_prompt_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[serde_json::Value]>,
    ) -> Result<PromptWithGrammar> {
        let chat_messages: Vec<LlamaChatMessage> = messages
            .iter()
            .map(|m| {
                LlamaChatMessage::new(m.role.clone(), m.content.clone())
                    .map_err(|e| Error::Llm(format!("invalid chat message: {e}")))
            })
            .collect::<Result<Vec<_>>>()?;

        match self.model.chat_template(None) {
            Ok(tmpl) => {
                if let Some(tool_defs) = tools {
                    if !tool_defs.is_empty() {
                        let tools_json = serde_json::to_string(tool_defs)
                            .map_err(|e| Error::Llm(format!("tools serialization: {e}")))?;
                        let result = self
                            .model
                            .apply_chat_template_with_tools_oaicompat(
                                &tmpl,
                                &chat_messages,
                                Some(&tools_json),
                                None,
                                true,
                            )
                            .map_err(|e| {
                                Error::Llm(format!("chat template with tools failed: {e}"))
                            })?;
                        return Ok(PromptWithGrammar {
                            prompt: result.prompt,
                            grammar: result.grammar,
                            grammar_lazy: result.grammar_lazy,
                        });
                    }
                }
                let prompt = self
                    .model
                    .apply_chat_template(&tmpl, &chat_messages, true)
                    .map_err(|e| Error::Llm(format!("chat template failed: {e}")))?;
                Ok(PromptWithGrammar {
                    prompt,
                    grammar: None,
                    grammar_lazy: false,
                })
            }
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
                Ok(PromptWithGrammar {
                    prompt,
                    grammar: None,
                    grammar_lazy: false,
                })
            }
        }
    }

    /// Run synchronous inference on the model.
    ///
    /// Creates a fresh context on the calling thread to satisfy Metal's
    /// thread affinity requirements. When a grammar is provided, it constrains
    /// the model output to valid tool-call JSON.
    fn generate(
        &self,
        prompt: &str,
        max_tokens: i32,
        temperature: f32,
        grammar: Option<&str>,
        grammar_lazy: bool,
    ) -> Result<(String, Usage)> {
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.context_length))
            .with_n_batch(self.context_length);
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
        // When a grammar is provided, add a grammar sampler to constrain tool-call output.
        let grammar_sampler = if let Some(grammar_str) = grammar {
            if grammar_lazy {
                // Lazy grammar: triggers on common tool-call markers
                let trigger_words: Vec<&[u8]> = vec![b"{\"name\"", b"<tool_call>", b"```json"];
                LlamaSampler::grammar_lazy(&self.model, grammar_str, "root", trigger_words, &[])
                    .ok()
            } else {
                LlamaSampler::grammar(&self.model, grammar_str, "root").ok()
            }
        } else {
            None
        };

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

        if let Some(gs) = grammar_sampler {
            sampler = LlamaSampler::chain_simple([sampler, gs]);
        }

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
        let tools = request.tools.as_deref();
        let pg = self.format_prompt_with_tools(&request.messages, tools)?;
        let max_tokens = request.max_tokens.unwrap_or(256) as i32;
        let temperature = request.temperature.unwrap_or(0.8) as f32;

        let (raw_output, usage) = self.generate(
            &pg.prompt,
            max_tokens,
            temperature,
            pg.grammar.as_deref(),
            pg.grammar_lazy,
        )?;

        // Try to parse tool calls from the output (OpenAI-compatible JSON format)
        let (content, tool_calls, finish_reason) = parse_tool_calls(&raw_output);

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
                    tool_calls,
                    ..Default::default()
                },
                finish_reason: Some(finish_reason),
            }],
            usage: Some(usage),
        })
    }

    async fn chat_stream(&self, request: &ChatCompletionRequest) -> Result<ChatStream> {
        let tools = request.tools.as_deref();
        let pg = self.format_prompt_with_tools(&request.messages, tools)?;
        let prompt = pg.prompt;
        // Note: grammar-constrained streaming not yet supported — would need per-token grammar sampling
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
                            content: if content.is_empty() {
                                None
                            } else {
                                Some(content)
                            },
                        },
                        finish_reason: finish,
                    }],
                };
                let _ = tx.blocking_send(Ok(chunk));
            };

            // Create context
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(context_length))
                .with_n_batch(context_length);
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

/// Parse tool calls from the model's raw output.
///
/// Qwen3.5 (and other models with Jinja tool templates) output tool calls as JSON.
/// The format varies by model but typically follows one of:
/// - A JSON object with `"name"` and `"arguments"` fields
/// - A JSON array of tool call objects
///
/// Returns (text_content, optional_tool_calls, finish_reason).
fn parse_tool_calls(raw: &str) -> (String, Option<Vec<super::router::ToolCall>>, String) {
    let trimmed = raw.trim();

    // Try to detect tool call JSON in the output.
    // Models typically output tool calls as JSON objects/arrays, sometimes wrapped in
    // <tool_call>...</tool_call> tags or similar markers.
    let json_str = if let Some(inner) = extract_between(trimmed, "<tool_call>", "</tool_call>") {
        inner
    } else if let Some(inner) = extract_between(trimmed, "```json\n", "\n```") {
        inner
    } else if trimmed.starts_with('{') || trimmed.starts_with('[') {
        trimmed
    } else {
        // No tool call detected — return as plain text
        return (raw.to_string(), None, "stop".into());
    };

    // Try parsing as a single tool call object
    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(json_str) {
        if let Some(calls) = parse_tool_call_value(&obj) {
            if !calls.is_empty() {
                return (String::new(), Some(calls), "tool_calls".into());
            }
        }
    }

    // Not valid tool call JSON — return as text
    (raw.to_string(), None, "stop".into())
}

fn parse_tool_call_value(val: &serde_json::Value) -> Option<Vec<super::router::ToolCall>> {
    use super::router::{ToolCall, ToolCallFunction};

    match val {
        serde_json::Value::Object(obj) => {
            let name = obj.get("name")?.as_str()?.to_string();
            let arguments = obj
                .get("arguments")
                .or_else(|| obj.get("parameters"))
                .map(|v| {
                    if v.is_string() {
                        v.as_str().unwrap_or("{}").to_string()
                    } else {
                        serde_json::to_string(v).unwrap_or_default()
                    }
                })
                .unwrap_or_else(|| "{}".to_string());

            Some(vec![ToolCall {
                id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                call_type: "function".into(),
                function: ToolCallFunction { name, arguments },
            }])
        }
        serde_json::Value::Array(arr) => {
            let calls: Vec<ToolCall> = arr
                .iter()
                .filter_map(parse_tool_call_value)
                .flatten()
                .collect();
            if calls.is_empty() {
                None
            } else {
                Some(calls)
            }
        }
        _ => None,
    }
}

/// Extract content between two markers.
fn extract_between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = s.find(start)? + start.len();
    let end_idx = s[start_idx..].find(end)? + start_idx;
    Some(s[start_idx..end_idx].trim())
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

        let path = download_gguf(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE).expect("download failed");

        let backend = get_or_init_backend().unwrap();
        // Force CPU-only (ngl=0) — Metal on x86_64 Mac produces incorrect results
        // with some quantization formats.
        let params = LlamaModelParams::default().with_n_gpu_layers(0);
        let model =
            LlamaModel::load_from_file(&backend, path, &params).expect("unable to load model");

        // Use model's chat template
        let tmpl = model.chat_template(None).expect("no chat template");
        let msgs =
            vec![
                LlamaChatMessage::new("user".into(), "What is 2+2? Answer briefly.".into())
                    .unwrap(),
            ];
        let prompt = model
            .apply_chat_template(&tmpl, &msgs, true)
            .expect("template failed");
        eprintln!("Prompt: {prompt:?}");

        let ctx_params = LlamaContextParams::default();
        let mut ctx = model
            .new_context(&backend, ctx_params)
            .expect("unable to create context");

        let tokens_list = model
            .str_to_token(&prompt, AddBos::Always)
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

            let piece = model
                .token_to_piece(token, &mut decoder, true, None)
                .unwrap();
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

        let pg = provider
            .format_prompt_with_tools(
                &[ChatMessage {
                    role: "user".into(),
                    content: "Explain what a binary tree is in 2-3 sentences.".into(),
                    ..Default::default()
                }],
                None,
            )
            .expect("format_prompt failed");

        eprintln!("Prompt: {:?}", pg.prompt);

        let (output, usage) = provider
            .generate(&pg.prompt, 128, 0.8, None, false)
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
            messages: vec![ChatMessage::text("user", "Say hello in exactly one word.")],
            temperature: Some(0.0),
            max_tokens: Some(16),
            ..Default::default()
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
