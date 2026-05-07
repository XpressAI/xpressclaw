# ADR-023: Ollama-only Local Inference

## Status
Accepted

## Context

ADR-011 proposed two paths for running models locally:

1. **Ollama** — HTTP proxy to a separate `ollama serve` process.
2. **Embedded llama.cpp** — `llama-cpp-2` Rust bindings linked into the CLI
   binary, with `cuda`/`metal` Cargo features per platform and a custom
   GGUF download flow during the setup wizard.

Both shipped. The embedded path turned out to be a persistent source of
problems:

- **Crashes.** `llama-cpp-2` invariant violations (KV-cache type mismatches,
  sampler lifetime issues) brought down the whole server process. Tracking
  these required deep familiarity with llama.cpp internals.
- **Hardware-specific build complexity.** Releasing the desktop bundle
  required cross-compiling with the right combination of features for each
  platform: `metal` for Apple Silicon, `cuda` for Linux+NVIDIA, plain CPU
  fallback elsewhere. CI had to detect-and-pass `--features` based on the
  build host. CUDA in particular needed bespoke `CUDA_PATH`/`RUSTFLAGS`
  setup in `build.sh` to find `libcudart_static.a` across distros.
- **GGUF download UX.** The wizard had its own hf-hub-backed downloader
  with a progress bar, separate from the rest of the model-management story.
  When download failed mid-way the recovery story was unclear.
- **Two ways to run the same model.** `provider: local` (embedded) and
  `provider: ollama` (HTTP) both mapped to "run Qwen3 locally," and the
  router needed a separate arm for each. The `xpressai.yaml` shape had
  `model_path` purely to support the embedded path.

Ollama solves all of this for us:

- Ollama maintains its own native builds with GPU acceleration per platform.
- It already exposes a standard HTTP API (`/api/pull`, `/api/tags`,
  `/v1/chat/completions`) and we already speak it.
- It handles model storage, quantization choice, and download progress.
- Crashes happen in a separate process — the server stays up.

## Decision

Embedded llama.cpp is removed entirely. Local inference goes through Ollama.

Removed:

- `crates/xpressclaw-core/src/llm/llamacpp.rs` and the `LazyLlamaCppProvider`.
- `local-llm`, `metal`, `cuda` Cargo features (in `xpressclaw-core`,
  `xpressclaw-server`, `xpressclaw-cli`).
- `llama-cpp-2`, `hf-hub`, `encoding_rs` workspace dependencies.
- The `provider: local` arm in `LlmRouter::materialize_provider` and the
  `model_path` field on `AgentLlmConfig`.
- The GGUF download flow: `download_gguf_with_progress`, `is_gguf_cached`,
  `resolve_gguf_source`, the `download_progress` state, the
  `GET /api/setup/download-status` route, the `use_embedded` request flag,
  and the wizard's download-progress polling UI.
- `build.sh` detection of `nvcc` / `metal` and the `--features` passing.

Kept:

- `provider: ollama` for HTTP-based local inference.
- The reconciler's per-agent Ollama pull loop (`reconcile_models`), which
  walks every Ollama-using agent and pulls missing models from each agent's
  declared `base_url` in the background.
- Hardware detection (`system::detect`, `system::recommend_model`) — the
  recommendations now feed an Ollama tag picker instead of a GGUF picker.

## Consequences

### Positive

- The CLI binary is smaller and builds without GPU SDKs.
- Release CI no longer needs CUDA toolkits or Metal-specific cross-compile
  steps — it's a plain `cargo build --release`.
- No more in-process LLM crashes taking down the server.
- One way to run a local model. The `xpressai.yaml` shape is simpler.

### Negative

- Users must install Ollama separately. The "single binary, no
  dependencies" zero-config story no longer covers local inference. The
  setup wizard surfaces the install link; the agent stays unable to chat
  until Ollama is running and the model is pulled.
- Custom XpressAI models (e.g.
  [`XpressAI/Qwen3.6-27B-RYS-UD-Q4_K_XL-GGUF`](https://huggingface.co/XpressAI/Qwen3.6-27B-RYS-UD-Q4_K_XL-GGUF))
  on HuggingFace can't be loaded directly anymore. To use them, we need to
  publish them to the Ollama Hub first (separate work — see "Future" below).

### Future

- **Republish XpressAI custom models on Ollama Hub.** Our HuggingFace
  org ([`XpressAI`](https://huggingface.co/XpressAI)) has GGUF builds that
  outperform the base Qwen3.5/3.6 models. Once republished as Ollama
  models (`xpressai/qwen3.6-27b-rys` or similar), agents can pull them
  with the same `provider: ollama` path — no code changes required.
- **First-class pull-with-progress endpoint.** Today the reconciler pulls
  silently in the background. A dedicated `POST /api/setup/pull-model` with
  SSE progress would give the wizard a "pulling X of Y MB" UI without
  reintroducing the embedded download path.

## Migration

Existing `xpressai.yaml` files using `provider: local` will warn at
router-build time and the agent will be skipped:

```
unknown provider type 'local'. Supported providers: openai, anthropic, ollama.
```

Users edit the agent's `llm` block to `provider: ollama`, set the
appropriate `model:` tag, and run `xpressclaw up`. The reconciler pulls
the model from Ollama on first start.

## Related ADRs

- ADR-011: Default Local Model (superseded by this ADR)
- ADR-018: Desired-State Controller (the reconciler that pulls models)
