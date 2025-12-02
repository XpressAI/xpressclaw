"""XpressAI init command - Initialize a new workspace."""

from pathlib import Path
import click

from xpressai.core.config import DEFAULT_CONFIG_TEMPLATE


def run_init(backend: str = "claude-code", force: bool = False) -> None:
    """Initialize a new XpressAI workspace."""
    config_path = Path.cwd() / "xpressai.yaml"

    # Check if already initialized
    if config_path.exists() and not force:
        click.echo(click.style("Warning: xpressai.yaml already exists.", fg="yellow"))
        click.echo("Use --force to overwrite.")
        return

    click.echo(click.style("Initializing XpressAI workspace...", fg="cyan", bold=True))

    # Detect GPU and recommend model
    gpu_info = _detect_gpu()
    if gpu_info["available"]:
        click.echo(f"Found GPU: {gpu_info['name']} ({gpu_info['vram_gb']:.1f}GB)")
        if backend == "local":
            click.echo(f"  Recommended quantization: {gpu_info['recommended_quant']}")
    else:
        click.echo(click.style("No GPU detected.", fg="yellow"))
        if backend == "local":
            click.echo("  Local inference will be slow. Consider using --backend=claude-code")

    # Write config file
    config_content = DEFAULT_CONFIG_TEMPLATE.format(backend=backend)
    config_path.write_text(config_content)
    click.echo(f"Created {config_path}")

    # Create workspace directory
    workspace_dir = Path.home() / "agent-workspace"
    workspace_dir.mkdir(exist_ok=True)
    click.echo(f"Created workspace: {workspace_dir}")

    # Create XpressAI data directory
    data_dir = Path.home() / ".xpressai"
    data_dir.mkdir(exist_ok=True)
    click.echo(f"Created data directory: {data_dir}")

    # Show next steps
    click.echo()
    click.echo(click.style("Ready!", fg="green", bold=True))
    click.echo()
    click.echo("Next steps:")
    click.echo("  1. Edit xpressai.yaml to customize your agent")

    if backend == "claude-code":
        click.echo("  2. Set your API key: export ANTHROPIC_API_KEY=sk-ant-...")
        click.echo("  3. Start the runtime: xpressai up")
    elif backend == "local":
        click.echo("  2. Start vLLM server: vllm serve Qwen/Qwen3-8B")
        click.echo("  3. Start the runtime: xpressai up")
        click.echo()
        click.echo("  Or use Ollama instead:")
        click.echo("    - Set inference_backend: ollama in xpressai.yaml")
        click.echo("    - Run: ollama pull qwen3:8b")
    else:
        click.echo("  2. Configure your backend credentials")
        click.echo("  3. Start the runtime: xpressai up")

    click.echo()
    click.echo("Or just run: xpressai up")


def _detect_gpu() -> dict:
    """Detect available GPU."""
    result = {
        "available": False,
        "name": None,
        "vram_gb": 0,
        "recommended_quant": "q4_k_m",
    }

    # Try CUDA via torch
    try:
        import torch

        if torch.cuda.is_available():
            result["available"] = True
            result["name"] = torch.cuda.get_device_name(0)
            result["vram_gb"] = torch.cuda.get_device_properties(0).total_memory / 1e9

            if result["vram_gb"] >= 12:
                result["recommended_quant"] = "q6_k"
            elif result["vram_gb"] >= 10:
                result["recommended_quant"] = "q5_k_m"
            return result
    except ImportError:
        pass

    # Try Apple Silicon
    try:
        import platform

        if platform.processor() == "arm" and platform.system() == "Darwin":
            result["available"] = True
            result["name"] = "Apple Silicon"
            result["vram_gb"] = 8  # Conservative estimate
            return result
    except:
        pass

    return result
