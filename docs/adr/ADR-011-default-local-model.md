# ADR-011: Default Local Model (Qwen3-8B)

## Status
Accepted

## Context

XpressAI's "zero configuration" philosophy requires a default that works without:
- API keys
- Cloud accounts
- Ongoing costs

Local models solve this, but we need to choose one that:
- Runs on consumer hardware (8GB+ GPU)
- Has good reasoning and tool use
- Supports long context
- Is easy to deploy

## Decision

We will use **Qwen3-8B** as the default local model.

### Why Qwen3-8B?

1. **State-of-the-art at 8B**: Outperforms many larger models
2. **Thinking mode**: Built-in `/think` and `/no_think` for reasoning vs. quick responses
3. **Strong tool use**: Explicitly trained for agent capabilities
4. **32K context native**: Room for memory, tools, and conversation
5. **GGUF available**: Easy deployment via llama.cpp/Ollama
6. **Multilingual**: 100+ languages
7. **Open weights**: Apache 2.0 license

### Hardware Requirements

| Quantization | VRAM   | Quality     | Speed       |
|-------------|--------|-------------|-------------|
| Q4_K_M      | ~5GB   | Good        | Fast        |
| Q5_K_M      | ~6GB   | Better      | Good        |
| Q6_K        | ~7GB   | Very Good   | Good        |
| Q8_0        | ~9GB   | Best        | Slower      |

**Default**: Q4_K_M for 8GB GPUs, Q6_K for 12GB+

### Inference Options

We support two backends for local inference:

#### 1. Ollama (Recommended)

```python
class OllamaBackend:
    """Local inference via Ollama."""
    
    def __init__(self, model: str = "qwen3:8b"):
        self.model = model
        self.base_url = "http://localhost:11434"
    
    async def generate(
        self, 
        messages: list[dict],
        tools: list[dict] | None = None,
        think: bool = False
    ) -> AsyncIterator[str]:
        # Add thinking mode toggle
        if think and messages:
            messages[-1]["content"] += " /think"
        
        async with aiohttp.ClientSession() as session:
            async with session.post(
                f"{self.base_url}/api/chat",
                json={
                    "model": self.model,
                    "messages": messages,
                    "tools": tools,
                    "stream": True,
                    "options": {
                        "temperature": 0.6 if think else 0.7,
                        "top_p": 0.95 if think else 0.8,
                        "top_k": 20,
                        "repeat_penalty": 1.5,  # Important for quantized models
                    }
                }
            ) as response:
                async for line in response.content:
                    data = json.loads(line)
                    if "message" in data:
                        yield data["message"]["content"]
```

#### 2. llama.cpp (Direct)

```python
class LlamaCppBackend:
    """Direct llama.cpp inference."""
    
    def __init__(self, model_path: str):
        from llama_cpp import Llama
        
        self.llm = Llama(
            model_path=model_path,
            n_gpu_layers=-1,  # Use all GPU layers
            n_ctx=32768,
            verbose=False,
        )
    
    async def generate(
        self,
        messages: list[dict],
        tools: list[dict] | None = None,
        think: bool = False
    ) -> AsyncIterator[str]:
        prompt = self._format_chat(messages, tools, think)
        
        for output in self.llm(
            prompt,
            max_tokens=4096,
            temperature=0.6 if think else 0.7,
            top_p=0.95 if think else 0.8,
            top_k=20,
            repeat_penalty=1.5,
            stream=True,
        ):
            yield output["choices"][0]["text"]
```

### Model Download and Setup

```python
class LocalModelManager:
    """Manages local model downloads and setup."""
    
    MODELS = {
        "qwen3-8b": {
            "ollama": "qwen3:8b",
            "gguf": {
                "repo": "Qwen/Qwen3-8B-GGUF",
                "files": {
                    "q4_k_m": "qwen3-8b-q4_k_m.gguf",
                    "q5_k_m": "qwen3-8b-q5_k_m.gguf",
                    "q6_k": "qwen3-8b-q6_k.gguf",
                    "q8_0": "qwen3-8b-q8_0.gguf",
                }
            }
        }
    }
    
    def __init__(self, cache_dir: Path | None = None):
        self.cache_dir = cache_dir or Path.home() / ".xpressai" / "models"
        self.cache_dir.mkdir(parents=True, exist_ok=True)
    
    async def ensure_model(
        self, 
        model: str = "qwen3-8b",
        quantization: str = "q4_k_m"
    ) -> Path | str:
        """Ensure model is available, downloading if needed."""
        
        # Check for Ollama first
        if await self._check_ollama():
            model_name = self.MODELS[model]["ollama"]
            if await self._ollama_has_model(model_name):
                return model_name
            
            # Pull model
            print(f"Downloading {model_name} via Ollama...")
            await self._ollama_pull(model_name)
            return model_name
        
        # Fall back to direct GGUF download
        gguf_info = self.MODELS[model]["gguf"]
        filename = gguf_info["files"][quantization]
        local_path = self.cache_dir / filename
        
        if local_path.exists():
            return local_path
        
        # Download from HuggingFace
        print(f"Downloading {filename}...")
        await self._download_gguf(gguf_info["repo"], filename, local_path)
        
        return local_path
    
    async def _check_ollama(self) -> bool:
        """Check if Ollama is running."""
        try:
            async with aiohttp.ClientSession() as session:
                async with session.get("http://localhost:11434/api/tags") as resp:
                    return resp.status == 200
        except:
            return False
    
    async def _ollama_pull(self, model: str) -> None:
        """Pull a model via Ollama."""
        process = await asyncio.create_subprocess_exec(
            "ollama", "pull", model,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE
        )
        await process.wait()
    
    async def _download_gguf(
        self, 
        repo: str, 
        filename: str, 
        local_path: Path
    ) -> None:
        """Download GGUF from HuggingFace."""
        from huggingface_hub import hf_hub_download
        
        hf_hub_download(
            repo_id=repo,
            filename=filename,
            local_dir=self.cache_dir,
            local_dir_use_symlinks=False,
        )
```

### Thinking Mode Integration

Qwen3 supports `/think` and `/no_think` modes. We leverage this:

```python
class SmartThinkingBackend:
    """Automatically decides when to use thinking mode."""
    
    async def send(self, message: str, task_complexity: str = "auto") -> AsyncIterator[str]:
        # Determine if thinking is needed
        use_thinking = self._should_think(message, task_complexity)
        
        async for chunk in self.backend.generate(
            messages=[{"role": "user", "content": message}],
            think=use_thinking
        ):
            # Strip thinking tags from output if present
            yield self._clean_thinking(chunk)
    
    def _should_think(self, message: str, complexity: str) -> bool:
        if complexity == "always":
            return True
        if complexity == "never":
            return False
        
        # Auto-detect based on message content
        thinking_triggers = [
            "step by step",
            "analyze",
            "debug",
            "complex",
            "figure out",
            "solve",
            "plan",
        ]
        
        return any(trigger in message.lower() for trigger in thinking_triggers)
```

### GPU Detection

```python
class GPUDetector:
    """Detects available GPU and recommends quantization."""
    
    @staticmethod
    def detect() -> dict:
        """Detect GPU capabilities."""
        result = {
            "available": False,
            "name": None,
            "vram_gb": 0,
            "recommended_quant": "q4_k_m",
        }
        
        try:
            import torch
            if torch.cuda.is_available():
                result["available"] = True
                result["name"] = torch.cuda.get_device_name(0)
                result["vram_gb"] = torch.cuda.get_device_properties(0).total_memory / 1e9
                
                # Recommend quantization based on VRAM
                if result["vram_gb"] >= 12:
                    result["recommended_quant"] = "q6_k"
                elif result["vram_gb"] >= 10:
                    result["recommended_quant"] = "q5_k_m"
                else:
                    result["recommended_quant"] = "q4_k_m"
        except ImportError:
            pass
        
        # Check for Apple Silicon
        try:
            import platform
            if platform.processor() == "arm" and platform.system() == "Darwin":
                result["available"] = True
                result["name"] = "Apple Silicon"
                # Assume unified memory is available
                result["vram_gb"] = 8  # Conservative estimate
                result["recommended_quant"] = "q4_k_m"
        except:
            pass
        
        return result
```

### First-Run Experience

```python
async def setup_local_model() -> str:
    """Set up local model on first run."""
    
    # Detect GPU
    gpu = GPUDetector.detect()
    
    if not gpu["available"]:
        print("⚠️  No GPU detected. Local inference will be slow.")
        print("   Consider using a cloud backend: xpressai config set backend claude-code")
        response = input("Continue with CPU inference? [y/N] ")
        if response.lower() != "y":
            raise SystemExit(1)
    else:
        print(f"✓ Found GPU: {gpu['name']} ({gpu['vram_gb']:.1f}GB)")
        print(f"  Recommended quantization: {gpu['recommended_quant']}")
    
    # Download model
    manager = LocalModelManager()
    model_path = await manager.ensure_model(
        quantization=gpu["recommended_quant"]
    )
    
    print(f"✓ Model ready: {model_path}")
    
    return model_path
```

### Configuration

```yaml
# xpressai.yaml

agent:
  backend: local  # Default
  
  local:
    model: qwen3-8b
    quantization: auto  # auto | q4_k_m | q5_k_m | q6_k | q8_0
    inference: auto     # auto | ollama | llama.cpp
    
    # Performance tuning
    context_length: 32768
    batch_size: 512
    threads: auto
    
    # Thinking mode
    thinking: auto  # auto | always | never
```

### Upgrade Path

When users hit local model limitations:

```python
async def suggest_upgrade(agent_id: str, reason: str) -> None:
    """Suggest upgrading to cloud model."""
    
    message = f"""
I'm working on this task, but it's quite complex. 
I could do a better job with a more capable model.

Would you like to switch to Claude for this task?
You'll need to set ANTHROPIC_API_KEY.

[Yes, use Claude] [No, keep trying locally]
"""
    
    await send_to_user(agent_id, message)
```

## Consequences

### Positive
- Zero cost to get started
- No API keys required
- Works offline
- Privacy (data stays local)
- Good performance for many tasks

### Negative
- Requires decent hardware (8GB GPU)
- First download is ~5GB
- Less capable than cloud models
- May frustrate users expecting GPT-4 quality

### Implementation Notes

1. Check for Ollama first (simplest)
2. Fall back to llama.cpp for more control
3. Detect GPU and recommend quantization
4. Show download progress clearly
5. Gracefully handle no-GPU scenarios

## Related ADRs
- ADR-002: Agent Backend (local backend implementation)
- ADR-010: Budget Controls (local = $0 cost)
