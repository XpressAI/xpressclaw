use serde::Serialize;

/// Detected system hardware information.
#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub total_memory_gb: f64,
    pub available_memory_gb: f64,
    pub cpu_count: usize,
    pub gpu: GpuInfo,
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuInfo {
    pub available: bool,
    pub name: Option<String>,
    pub vram_gb: Option<f64>,
}

/// Model recommendation based on system hardware.
#[derive(Debug, Clone, Serialize)]
pub struct ModelRecommendation {
    pub model: String,
    pub embedding_model: String,
    pub reason: String,
    pub all_options: Vec<ModelOption>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelOption {
    pub model: String,
    pub display_name: String,
    pub ram_required_gb: f64,
    pub suitable: bool,
}

/// Detect system hardware (RAM, CPU, GPU).
pub fn detect() -> SystemInfo {
    use sysinfo::System;

    let mut sys = System::new_all();
    sys.refresh_all();

    let total_memory_gb = sys.total_memory() as f64 / (1024.0 * 1024.0 * 1024.0);
    let available_memory_gb = sys.available_memory() as f64 / (1024.0 * 1024.0 * 1024.0);
    let cpu_count = sys.cpus().len();

    let gpu = detect_gpu();

    SystemInfo {
        total_memory_gb,
        available_memory_gb,
        cpu_count,
        gpu,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

/// Recommend a Qwen3.5 model size based on available resources.
///
/// Selection logic:
/// - On Windows/Linux with a discrete GPU: choose based on GPU VRAM
/// - On macOS with Apple Silicon: choose based on system RAM (unified memory)
/// - On Intel Macs: max at 9B (Metal on Intel is unreliable)
/// - Otherwise: choose based on system RAM
///
/// Tier targets:
/// - <16GB: 4B
/// - 16GB+: 9B
/// - 32GB+ Apple Silicon: 27B
pub fn recommend_model(info: &SystemInfo) -> ModelRecommendation {
    let is_apple_silicon = info.os == "macos" && info.arch == "aarch64";
    let is_intel_mac = info.os == "macos" && info.arch == "x86_64";
    let has_discrete_gpu = info.gpu.available && info.gpu.vram_gb.is_some();

    // Determine the memory budget for model selection.
    // On GPU systems (Windows/Linux), use VRAM. Otherwise use system RAM.
    let budget_gb = if has_discrete_gpu && !is_apple_silicon {
        // Discrete GPU — use VRAM (leave ~1GB headroom)
        info.gpu.vram_gb.unwrap_or(0.0) - 1.0
    } else {
        // CPU or unified memory — use ~60% of total RAM
        info.total_memory_gb * 0.6
    };

    // Dense models are recommended automatically; MoE models are listed
    // but only used when the user explicitly selects them.
    let options = vec![
        // --- Qwen 3.5 (unsloth/Qwen3.5-*-GGUF) ---
        ModelOption {
            model: "qwen3.5:0.8b".into(),
            display_name: "Qwen 3.5 0.8B".into(),
            ram_required_gb: 1.0,
            suitable: budget_gb >= 1.0,
        },
        ModelOption {
            model: "qwen3.5:4b".into(),
            display_name: "Qwen 3.5 4B".into(),
            ram_required_gb: 4.0,
            suitable: budget_gb >= 4.0,
        },
        ModelOption {
            model: "qwen3.5:9b".into(),
            display_name: "Qwen 3.5 9B".into(),
            ram_required_gb: 8.0,
            suitable: budget_gb >= 8.0,
        },
        ModelOption {
            model: "qwen3.5:27b".into(),
            display_name: "Qwen 3.5 27B".into(),
            ram_required_gb: 20.0,
            suitable: budget_gb >= 20.0,
        },
        // Qwen 3.5 MoE (only used when user explicitly selects)
        ModelOption {
            model: "qwen3.5:35b-a3b".into(),
            display_name: "Qwen 3.5 35B-A3B (MoE)".into(),
            ram_required_gb: 24.0,
            suitable: false,
        },
        ModelOption {
            model: "qwen3.5:122b-a10b".into(),
            display_name: "Qwen 3.5 122B-A10B (MoE)".into(),
            ram_required_gb: 80.0,
            suitable: false,
        },
        ModelOption {
            model: "qwen3.5:397b-a17b".into(),
            display_name: "Qwen 3.5 397B-A17B (MoE)".into(),
            ram_required_gb: 240.0,
            suitable: false,
        },
        // --- Gemma 4 (unsloth/gemma-4-*-GGUF) ---
        ModelOption {
            model: "gemma4:e2b".into(),
            display_name: "Gemma 4 E2B".into(),
            ram_required_gb: 2.0,
            suitable: false, // not auto-recommended (Qwen is default)
        },
        ModelOption {
            model: "gemma4:e4b".into(),
            display_name: "Gemma 4 E4B".into(),
            ram_required_gb: 4.0,
            suitable: false,
        },
        ModelOption {
            model: "gemma4:26b-a4b".into(),
            display_name: "Gemma 4 26B-A4B (MoE)".into(),
            ram_required_gb: 18.0,
            suitable: false,
        },
        ModelOption {
            model: "gemma4:31b".into(),
            display_name: "Gemma 4 31B".into(),
            ram_required_gb: 22.0,
            suitable: false,
        },
    ];

    // Apply hardware caps:
    // Intel Macs: max at 9B (Metal is unreliable on x86_64)
    // The cap is expressed as max RAM budget in GB.
    let max_ram_gb = if is_intel_mac { 8.0 } else { f64::MAX };

    // Pick the largest suitable dense model that doesn't exceed the cap
    let recommended = options
        .iter()
        .rev()
        .find(|o| o.suitable && o.ram_required_gb <= max_ram_gb)
        .unwrap_or(&options[0]);

    let memory_source = if has_discrete_gpu && !is_apple_silicon {
        format!(
            "{:.0}GB VRAM ({})",
            info.gpu.vram_gb.unwrap_or(0.0),
            info.gpu.name.as_deref().unwrap_or("GPU")
        )
    } else {
        format!("{:.0}GB RAM", info.total_memory_gb)
    };

    let mut reason = format!("{memory_source} → {}", recommended.display_name);
    if is_intel_mac && budget_gb > max_ram_gb {
        reason.push_str(" (Intel Mac capped at 9B)");
    }

    ModelRecommendation {
        model: recommended.model.clone(),
        embedding_model: "nomic-embed-text".into(),
        reason,
        all_options: options,
    }
}

fn detect_gpu() -> GpuInfo {
    match std::env::consts::OS {
        "macos" => detect_gpu_macos(),
        "linux" => detect_gpu_linux(),
        _ => GpuInfo {
            available: false,
            name: None,
            vram_gb: None,
        },
    }
}

fn detect_gpu_macos() -> GpuInfo {
    // Apple Silicon always has Metal via unified memory
    if let Ok(output) = std::process::Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
    {
        let brand = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if brand.contains("Apple") {
            return GpuInfo {
                available: true,
                name: Some(format!("{brand} (Metal)")),
                vram_gb: None, // Unified memory — shared with system RAM
            };
        }
    }
    GpuInfo {
        available: false,
        name: None,
        vram_gb: None,
    }
}

fn detect_gpu_linux() -> GpuInfo {
    // Check for NVIDIA
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = text.lines().next() {
                let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                let name = parts.first().map(|s| s.to_string());
                let vram_gb = parts
                    .get(1)
                    .and_then(|s| s.parse::<f64>().ok())
                    .map(|mb| mb / 1024.0);
                return GpuInfo {
                    available: true,
                    name,
                    vram_gb,
                };
            }
        }
    }
    GpuInfo {
        available: false,
        name: None,
        vram_gb: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_returns_valid_info() {
        let info = detect();
        assert!(info.total_memory_gb > 0.0);
        assert!(info.cpu_count > 0);
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
    }

    #[test]
    fn test_recommend_model_low_ram() {
        let info = SystemInfo {
            total_memory_gb: 4.0,
            available_memory_gb: 2.0,
            cpu_count: 4,
            gpu: GpuInfo {
                available: false,
                name: None,
                vram_gb: None,
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let rec = recommend_model(&info);
        // 4 * 0.6 = 2.4 GB budget → 0.8B (needs 1GB)
        assert_eq!(rec.model, "qwen3.5:0.8b");
    }

    #[test]
    fn test_recommend_model_gpu_vram() {
        let info = SystemInfo {
            total_memory_gb: 64.0,
            available_memory_gb: 50.0,
            cpu_count: 16,
            gpu: GpuInfo {
                available: true,
                name: Some("NVIDIA RTX 4090".into()),
                vram_gb: Some(24.0),
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let rec = recommend_model(&info);
        // 24 - 1 = 23 GB VRAM budget → 27B (needs 20GB)
        assert_eq!(rec.model, "qwen3.5:27b");
    }

    #[test]
    fn test_recommend_model_16gb_apple_silicon() {
        let info = SystemInfo {
            total_memory_gb: 16.0,
            available_memory_gb: 10.0,
            cpu_count: 8,
            gpu: GpuInfo {
                available: true,
                name: Some("Apple M2 Pro (Metal)".into()),
                vram_gb: None,
            },
            os: "macos".into(),
            arch: "aarch64".into(),
        };
        let rec = recommend_model(&info);
        // 16 * 0.6 = 9.6 GB budget → 9B (needs 8GB)
        assert_eq!(rec.model, "qwen3.5:9b");
    }

    #[test]
    fn test_recommend_model_36gb_apple_silicon() {
        let info = SystemInfo {
            total_memory_gb: 36.0,
            available_memory_gb: 28.0,
            cpu_count: 10,
            gpu: GpuInfo {
                available: true,
                name: Some("Apple M2 Max (Metal)".into()),
                vram_gb: None,
            },
            os: "macos".into(),
            arch: "aarch64".into(),
        };
        let rec = recommend_model(&info);
        // 36 * 0.6 = 21.6 GB budget → 27B (needs 20GB)
        assert_eq!(rec.model, "qwen3.5:27b");
    }

    #[test]
    fn test_recommend_model_intel_mac_capped() {
        let info = SystemInfo {
            total_memory_gb: 64.0,
            available_memory_gb: 50.0,
            cpu_count: 16,
            gpu: GpuInfo {
                available: false,
                name: None,
                vram_gb: None,
            },
            os: "macos".into(),
            arch: "x86_64".into(),
        };
        let rec = recommend_model(&info);
        // 64 * 0.6 = 38.4 GB budget, but Intel Mac capped at 9B
        assert_eq!(rec.model, "qwen3.5:9b");
        assert!(rec.reason.contains("capped at 9B"));
    }

    #[test]
    fn test_recommend_model_small_gpu() {
        let info = SystemInfo {
            total_memory_gb: 16.0,
            available_memory_gb: 10.0,
            cpu_count: 8,
            gpu: GpuInfo {
                available: true,
                name: Some("NVIDIA GTX 1660".into()),
                vram_gb: Some(6.0),
            },
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        let rec = recommend_model(&info);
        // 6 - 1 = 5 GB VRAM budget → 4B (needs 4GB)
        assert_eq!(rec.model, "qwen3.5:4b");
    }
}
