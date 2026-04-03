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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
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
/// 128k supports tool-calling (Claude SDK sends ~18k tokens
/// of tool definitions) with ample room for conversation.
const DEFAULT_CONTEXT_LENGTH: u32 = 131_072;

/// Global LlamaBackend singleton — llama.cpp only allows one backend per process.
/// Uses a Mutex to prevent race conditions during initialization.
static LLAMA_BACKEND: std::sync::OnceLock<Arc<LlamaBackend>> = std::sync::OnceLock::new();
static LLAMA_INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn get_or_init_backend() -> Result<Arc<LlamaBackend>> {
    if let Some(b) = LLAMA_BACKEND.get() {
        return Ok(b.clone());
    }
    // Hold lock during init to prevent concurrent LlamaBackend::init() calls
    let _guard = LLAMA_INIT_LOCK.lock().unwrap();
    // Re-check after acquiring lock (another thread may have initialized)
    if let Some(b) = LLAMA_BACKEND.get() {
        return Ok(b.clone());
    }
    // Enable CUDA unified memory so the KV cache can spill to system RAM
    // when VRAM is insufficient (instead of failing with cudaMalloc OOM).
    if std::env::var("GGML_CUDA_ENABLE_UNIFIED_MEMORY").is_err() {
        std::env::set_var("GGML_CUDA_ENABLE_UNIFIED_MEMORY", "1");
    }
    let backend =
        LlamaBackend::init().map_err(|e| Error::Llm(format!("llama backend init failed: {e}")))?;
    let arc = Arc::new(backend);
    let _ = LLAMA_BACKEND.set(arc);
    Ok(LLAMA_BACKEND.get().unwrap().clone())
}

/// Prompt with optional grammar for constrained tool-call decoding.
struct PromptWithGrammar {
    prompt: String,
    grammar: Option<String>,
    grammar_lazy: bool,
    /// Parsed template result for response parsing (tool calls, thinking blocks).
    template_result: Option<llama_cpp_2::model::ChatTemplateResult>,
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
            // Offload all layers to GPU (CUDA/Metal)
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            let p = p.with_n_gpu_layers(u32::MAX);
            // Intel Macs have no Metal support
            #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
            let p = p.with_n_gpu_layers(0);
            p
        };
        let model = Arc::new(
            LlamaModel::load_from_file(&backend, path, &params)
                .map_err(|e| Error::Llm(format!("failed to load model: {e}")))?,
        );

        let n_ctx_train = model.n_ctx_train();
        // Context length stored for prompt-fits-in-context checks.
        // Actual context allocation uses auto-fit (n_ctx=0) at inference time.
        let context_length = if n_ctx_train > 0 {
            n_ctx_train as u32
        } else {
            DEFAULT_CONTEXT_LENGTH
        };
        tracing::info!(model = model_name, n_ctx_train, "GGUF model loaded");

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
    /// Uses the Jinja-based chat template API (`apply_chat_template_oaicompat`)
    /// which properly renders tools, thinking blocks, and tool call grammars
    /// for models like Qwen3.5. This is equivalent to `llama-server --jinja`.
    fn format_prompt_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[serde_json::Value]>,
    ) -> Result<PromptWithGrammar> {
        use llama_cpp_2::openai::OpenAIChatTemplateParams;

        // Convert messages to OpenAI-compatible JSON
        let messages_json: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let mut obj = serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                });
                if let Some(ref tc) = m.tool_calls {
                    obj["tool_calls"] = serde_json::to_value(tc).unwrap_or_default();
                }
                if let Some(ref tc_id) = m.tool_call_id {
                    obj["tool_call_id"] = serde_json::Value::String(tc_id.clone());
                }
                obj
            })
            .collect();
        let messages_str = serde_json::to_string(&messages_json)
            .map_err(|e| Error::Llm(format!("messages serialization: {e}")))?;

        let tools_str = tools
            .filter(|t| !t.is_empty())
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| Error::Llm(format!("tools serialization: {e}")))?;

        match self.model.chat_template(None) {
            Ok(tmpl) => {
                // Disable thinking for models 9B and smaller — they waste tokens
                // on low-quality reasoning that hurts more than it helps.
                let n_params = self.model.n_params();
                let enable_thinking = n_params > 10_000_000_000; // >10B
                if !enable_thinking {
                    tracing::debug!(
                        n_params,
                        "thinking disabled for small model (<= 10B params)"
                    );
                }

                let params = OpenAIChatTemplateParams {
                    messages_json: &messages_str,
                    tools_json: tools_str.as_deref(),
                    tool_choice: None,
                    json_schema: None,
                    grammar: None,
                    reasoning_format: if enable_thinking {
                        Some("deepseek")
                    } else {
                        None
                    },
                    chat_template_kwargs: None,
                    add_generation_prompt: true,
                    use_jinja: true,
                    parallel_tool_calls: false,
                    enable_thinking,
                    add_bos: false,
                    add_eos: false,
                    parse_tool_calls: true,
                };

                let result = self
                    .model
                    .apply_chat_template_oaicompat(&tmpl, &params)
                    .map_err(|e| Error::Llm(format!("chat template failed: {e}")))?;

                Ok(PromptWithGrammar {
                    prompt: result.prompt.clone(),
                    grammar: result.grammar.clone(),
                    grammar_lazy: result.grammar_lazy,
                    template_result: Some(result),
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
                    template_result: None,
                })
            }
        }
    }

    /// Run synchronous inference on the model.
    ///
    /// Creates a fresh context on the calling thread to satisfy Metal's
    /// thread affinity requirements. When a grammar is provided, it constrains
    /// the model output to valid tool-call JSON.
    #[allow(clippy::too_many_arguments)]
    fn generate(
        &self,
        prompt: &str,
        max_tokens: i32,
        temperature: f32,
        top_k: i32,
        top_p: f32,
        reasoning_budget: Option<i32>,
        grammar: Option<&str>,
        grammar_lazy: bool,
    ) -> Result<(String, Usage)> {
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(None)
            .with_n_batch(self.context_length)
            .with_type_k(llama_cpp_2::context::params::KvCacheType::Q8_0)
            .with_type_v(llama_cpp_2::context::params::KvCacheType::Q8_0);
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

        // Grammar-constrained decoding is disabled for now.
        // The Jinja template already guides the model to produce tool calls in the
        // correct format, and the response parser (parse_response_oaicompat) handles
        // extraction. Enabling grammar can cause SIGABRT in llama.cpp with some
        // model/grammar combinations.
        let grammar_sampler: Option<LlamaSampler> = None;
        let _ = (grammar, grammar_lazy); // suppress unused warnings

        let mut sampler = if temperature > 0.0 {
            LlamaSampler::chain_simple([
                LlamaSampler::penalties(64, 1.0, 0.0, 0.0),
                LlamaSampler::top_k(top_k),
                LlamaSampler::top_p(top_p, 1),
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
        let mut thinking_tokens = 0i32;
        let mut in_thinking = false;
        let budget = reasoning_budget.unwrap_or(i32::MAX);

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

            // Track thinking block tokens and enforce reasoning budget.
            if output.contains("<think>") && !output.contains("</think>") {
                if !in_thinking {
                    in_thinking = true;
                    thinking_tokens = 0;
                }
                thinking_tokens += 1;
                if thinking_tokens >= budget {
                    output.push_str("</think>\n");
                    in_thinking = false;
                    tracing::debug!(
                        thinking_tokens,
                        budget,
                        "reasoning budget reached, closing thinking block"
                    );
                }
            } else if in_thinking && output.contains("</think>") {
                in_thinking = false;
            }

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
                ..Default::default()
            },
        ))
    }
}

#[async_trait::async_trait]
impl LlmProvider for LlamaCppProvider {
    async fn chat(&self, request: &ChatCompletionRequest) -> Result<ChatCompletionResponse> {
        let tools = request.tools.as_deref();
        let pg = self.format_prompt_with_tools(&request.messages, tools)?;
        let max_tokens = request.max_tokens.unwrap_or(4096) as i32;

        // Per-model sampling defaults.
        let name_lower = self.model_name.to_lowercase();
        let is_gemma = name_lower.contains("gemma");
        let (temperature, top_k, top_p) = if is_gemma {
            // Google's Gemma 4 recommendations
            (request.temperature.unwrap_or(1.0) as f32, 64, 0.95f32)
        } else {
            // Qwen 3.5 / default
            (request.temperature.unwrap_or(0.8) as f32, 20, 0.95f32)
        };
        // Per-request reasoning budget (default 4096 tokens for thinking)
        let reasoning_budget = request.reasoning_budget.map(|b| b as i32).or(Some(4096));

        // Grammar-constrained decoding can crash with some model/grammar combinations.
        // Use it when available but fall back gracefully.
        let (raw_output, usage) = self.generate(
            &pg.prompt,
            max_tokens,
            temperature,
            top_k,
            top_p,
            reasoning_budget,
            pg.grammar.as_deref(),
            pg.grammar_lazy,
        )?;

        // Use the template's built-in response parser if available (handles tool calls
        // and thinking blocks according to the model's chat format). Falls back to
        // our manual parser for models without Jinja template support.
        let (content, tool_calls, finish_reason, reasoning_content) =
            if let Some(ref tmpl_result) = pg.template_result {
                match tmpl_result.parse_response_oaicompat(&raw_output, false) {
                    Ok(parsed_json) => parse_oaicompat_response(&parsed_json),
                    Err(_) => {
                        let (c, tc, fr) = parse_tool_calls(&raw_output);
                        (c, tc, fr, None)
                    }
                }
            } else {
                let (c, tc, fr) = parse_tool_calls(&raw_output);
                (c, tc, fr, None)
            };

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
                    reasoning_content,
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

        let name_lower = self.model_name.to_lowercase();
        let is_gemma = name_lower.contains("gemma");
        let temperature = if is_gemma {
            request.temperature.unwrap_or(1.0) as f32
        } else {
            request.temperature.unwrap_or(0.8) as f32
        };

        let has_tools = request.tools.is_some();
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
                            ..Default::default()
                        },
                        finish_reason: finish,
                    }],
                };
                let _ = tx.blocking_send(Ok(chunk));
            };

            // Create context
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(None)
                .with_n_batch(context_length)
                .with_type_k(llama_cpp_2::context::params::KvCacheType::Q8_0)
                .with_type_v(llama_cpp_2::context::params::KvCacheType::Q8_0);
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

            let stream_top_k = if is_gemma { 64 } else { 20 };
            let mut sampler = if temperature > 0.0 {
                LlamaSampler::chain_simple([
                    LlamaSampler::penalties(64, 1.0, 0.0, 0.0),
                    LlamaSampler::top_k(stream_top_k),
                    LlamaSampler::top_p(0.95, 1),
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

                // When tools are defined, buffer output so we can parse
                // <tool_call> tags at the end. Otherwise stream immediately.
                if !has_tools {
                    send_chunk(piece, None);
                }

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

            if has_tools {
                // Parse tool calls from buffered output
                let (parsed_content, tool_calls, finish_reason) = parse_tool_calls(&full_output);
                if let Some(ref tcs) = tool_calls {
                    // Emit text content first if any
                    if !parsed_content.is_empty() {
                        send_chunk(parsed_content, None);
                    }
                    // Emit structured tool call chunk
                    let chunk = super::router::ChatCompletionChunk {
                        id: id_clone.clone(),
                        object: "chat.completion.chunk".into(),
                        created,
                        model: model_name_clone.clone(),
                        choices: vec![super::router::ChunkChoice {
                            index: 0,
                            delta: super::router::ChunkDelta {
                                role: None,
                                content: None,
                                tool_calls: Some(
                                    tcs.iter()
                                        .enumerate()
                                        .map(|(i, tc)| super::router::ChunkToolCall {
                                            index: i as i64,
                                            id: Some(tc.id.clone()),
                                            call_type: Some("function".into()),
                                            function: Some(super::router::ChunkToolCallFunction {
                                                name: Some(tc.function.name.clone()),
                                                arguments: Some(tc.function.arguments.clone()),
                                            }),
                                        })
                                        .collect(),
                                ),
                                ..Default::default()
                            },
                            finish_reason: Some(finish_reason),
                        }],
                    };
                    let _ = tx.blocking_send(Ok(chunk));
                } else {
                    // No tool calls found — send buffered text as one chunk
                    send_chunk(full_output, Some("stop".into()));
                }
            } else {
                // No tools — final chunk with stop
                send_chunk(String::new(), Some("stop".into()));
            }
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

/// Parse the OAI-compat response JSON from the template's built-in parser.
///
/// The parser returns a JSON object with `content`, `tool_calls`, and
/// `reasoning_content` fields — matching the OpenAI chat completion format.
fn parse_oaicompat_response(
    json_str: &str,
) -> (
    String,
    Option<Vec<super::router::ToolCall>>,
    String,
    Option<String>,
) {
    use super::router::{ToolCall, ToolCallFunction};

    let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return (json_str.to_string(), None, "stop".into(), None);
    };

    let content = val["content"].as_str().unwrap_or("").to_string();
    let reasoning = val["reasoning_content"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let tool_calls = val["tool_calls"].as_array().and_then(|arr| {
        let calls: Vec<ToolCall> = arr
            .iter()
            .filter_map(|tc| {
                let id = tc["id"]
                    .as_str()
                    .unwrap_or(&format!("call_{}", uuid::Uuid::new_v4().simple()))
                    .to_string();
                let name = tc["function"]["name"].as_str()?.to_string();
                let arguments = tc["function"]["arguments"]
                    .as_str()
                    .map(|s| s.to_string())
                    .or_else(|| serde_json::to_string(&tc["function"]["arguments"]).ok())
                    .unwrap_or_else(|| "{}".to_string());

                Some(ToolCall {
                    id,
                    call_type: "function".into(),
                    function: ToolCallFunction { name, arguments },
                })
            })
            .collect();
        if calls.is_empty() {
            None
        } else {
            Some(calls)
        }
    });

    let finish_reason = if tool_calls.is_some() {
        "tool_calls"
    } else {
        "stop"
    }
    .to_string();

    (content, tool_calls, finish_reason, reasoning)
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
        // No JSON-style tool call detected — try XML format:
        // <tool_call>\n<function=name>\n<parameter=key>\nvalue\n</parameter>\n</function>\n</tool_call>
        if let Some(calls) = parse_xml_tool_calls(trimmed) {
            if !calls.is_empty() {
                return (String::new(), Some(calls), "tool_calls".into());
            }
        }
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

    // JSON didn't parse — try XML format inside the <tool_call> tags
    if let Some(inner) = extract_between(trimmed, "<tool_call>", "</tool_call>") {
        if let Some(calls) = parse_xml_tool_calls(inner) {
            if !calls.is_empty() {
                return (String::new(), Some(calls), "tool_calls".into());
            }
        }
    }

    // Not valid tool call — return as text
    (raw.to_string(), None, "stop".into())
}

/// Parse XML-style tool calls: `<function=name><parameter=key>value</parameter></function>`
/// This format is used by some model templates (e.g. Qwen3.5) during streaming.
fn parse_xml_tool_calls(raw: &str) -> Option<Vec<super::router::ToolCall>> {
    use super::router::{ToolCall, ToolCallFunction};

    let mut calls = Vec::new();
    let mut remaining = raw;

    while let Some(func_start) = remaining.find("<function=") {
        let after_prefix = &remaining[func_start + "<function=".len()..];
        let name_end = after_prefix.find('>')?;
        let name = after_prefix[..name_end].trim().to_string();
        let after_name = &after_prefix[name_end + 1..];

        // Parse parameters
        let mut args = serde_json::Map::new();
        let mut param_remaining = after_name;
        while let Some(param_start) = param_remaining.find("<parameter=") {
            let after_param = &param_remaining[param_start + "<parameter=".len()..];
            let param_name_end = after_param.find('>')?;
            let param_name = after_param[..param_name_end].trim().to_string();
            let after_param_name = &after_param[param_name_end + 1..];

            let param_value_end = after_param_name.find("</parameter>")?;
            let param_value = after_param_name[..param_value_end].trim();
            args.insert(
                param_name,
                serde_json::Value::String(param_value.to_string()),
            );

            param_remaining = &after_param_name[param_value_end + "</parameter>".len()..];
        }

        calls.push(ToolCall {
            id: format!("call_{}", uuid::Uuid::new_v4().simple()),
            call_type: "function".into(),
            function: ToolCallFunction {
                name,
                arguments: serde_json::to_string(&args).unwrap_or_else(|_| "{}".into()),
            },
        });

        // Advance past </function>
        remaining = match remaining[func_start..].find("</function>") {
            Some(end) => &remaining[func_start + end + "</function>".len()..],
            None => break,
        };
    }

    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
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
        let msgs = vec![llama_cpp_2::model::LlamaChatMessage::new(
            "user".into(),
            "What is 2+2? Answer briefly.".into(),
        )
        .unwrap()];
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
            .generate(&pg.prompt, 128, 0.8, 20, 0.95, Some(4096), None, false)
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

    /// Test that the model produces tool calls when given tools.
    #[ignore = "requires cached GGUF model"]
    #[tokio::test]
    async fn test_llamacpp_tool_calling() {
        let provider = LlamaCppProvider::from_huggingface(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE)
            .expect("failed to load model");

        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get the current weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string", "description": "City name" }
                    },
                    "required": ["city"]
                }
            }
        })];

        let request = ChatCompletionRequest {
            model: "qwen3.5:0.8b".into(),
            messages: vec![ChatMessage::text("user", "What's the weather in Tokyo?")],
            temperature: Some(0.0),
            max_tokens: Some(512),
            tools: Some(tools),
            ..Default::default()
        };

        let response = provider.chat(&request).await.expect("chat failed");
        let choice = &response.choices[0];

        eprintln!("Content: {:?}", choice.message.content);
        eprintln!("Tool calls: {:?}", choice.message.tool_calls);
        eprintln!("Reasoning: {:?}", choice.message.reasoning_content);
        eprintln!("Finish reason: {:?}", choice.finish_reason);

        // The model should produce a tool call for get_weather
        assert!(
            choice.message.tool_calls.is_some(),
            "expected tool call but got none. Content: {:?}",
            choice.message.content
        );
        let tool_calls = choice.message.tool_calls.as_ref().unwrap();
        assert!(!tool_calls.is_empty(), "tool calls array is empty");
        assert_eq!(
            tool_calls[0].function.name, "get_weather",
            "expected get_weather tool call"
        );

        // Arguments should contain "Tokyo"
        let args = &tool_calls[0].function.arguments;
        assert!(
            args.contains("Tokyo") || args.contains("tokyo"),
            "expected Tokyo in arguments: {args}"
        );
    }

    /// Test that the model produces thinking blocks when enabled.
    #[ignore = "requires cached GGUF model"]
    #[tokio::test]
    async fn test_llamacpp_thinking() {
        let provider = LlamaCppProvider::from_huggingface(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE)
            .expect("failed to load model");

        let request = ChatCompletionRequest {
            model: "qwen3.5:0.8b".into(),
            messages: vec![ChatMessage::text(
                "user",
                "What is 15 * 37? Think step by step.",
            )],
            temperature: Some(0.0),
            max_tokens: Some(512),
            ..Default::default()
        };

        let response = provider.chat(&request).await.expect("chat failed");
        let choice = &response.choices[0];

        eprintln!("Content: {:?}", choice.message.content);
        eprintln!("Reasoning: {:?}", choice.message.reasoning_content);

        // The model should produce either content or reasoning (or both).
        // With small models and limited tokens, thinking may consume all tokens.
        let has_content = !choice.message.content.is_empty();
        let has_reasoning = choice.message.reasoning_content.is_some();
        assert!(
            has_content || has_reasoning,
            "expected content or reasoning, got neither"
        );
        eprintln!("Has content: {has_content}, has reasoning: {has_reasoning}");
    }

    /// Test that format_prompt_with_tools produces a valid prompt with Jinja.
    #[ignore = "requires cached GGUF model"]
    #[test]
    fn test_llamacpp_jinja_template_with_tools() {
        let provider = LlamaCppProvider::from_huggingface(DEFAULT_GGUF_REPO, DEFAULT_GGUF_FILE)
            .expect("failed to load model");

        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "search",
                "description": "Search the web",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" }
                    },
                    "required": ["query"]
                }
            }
        })];

        let pg = provider
            .format_prompt_with_tools(
                &[ChatMessage::text("user", "Search for rust programming")],
                Some(&tools),
            )
            .expect("format_prompt failed");

        eprintln!("Prompt:\n{}", pg.prompt);
        eprintln!(
            "Grammar: {:?}",
            pg.grammar.as_deref().map(|g| &g[..100.min(g.len())])
        );
        eprintln!("Template result present: {}", pg.template_result.is_some());

        // Prompt should contain the tool definition
        assert!(
            pg.prompt.contains("search") || pg.prompt.contains("Search"),
            "prompt should contain tool name"
        );

        // Should have a template result for response parsing
        assert!(
            pg.template_result.is_some(),
            "template_result should be set"
        );

        // Grammar should be generated for tool calling
        assert!(
            pg.grammar.is_some(),
            "grammar should be generated for tools"
        );
    }
}
