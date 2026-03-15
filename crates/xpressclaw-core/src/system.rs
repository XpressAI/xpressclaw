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
pub fn recommend_model(info: &SystemInfo) -> ModelRecommendation {
    // Budget: use ~60% of total RAM for the model
    let ram_budget = info.total_memory_gb * 0.6;

    let options = vec![
        ModelOption {
            model: "qwen3.5:0.6b".into(),
            display_name: "Qwen 3.5 0.6B".into(),
            ram_required_gb: 1.0,
            suitable: ram_budget >= 1.0,
        },
        ModelOption {
            model: "qwen3.5:1.5b".into(),
            display_name: "Qwen 3.5 1.5B".into(),
            ram_required_gb: 2.0,
            suitable: ram_budget >= 2.0,
        },
        ModelOption {
            model: "qwen3.5:4b".into(),
            display_name: "Qwen 3.5 4B".into(),
            ram_required_gb: 4.0,
            suitable: ram_budget >= 4.0,
        },
        ModelOption {
            model: "qwen3.5:8b".into(),
            display_name: "Qwen 3.5 8B".into(),
            ram_required_gb: 8.0,
            suitable: ram_budget >= 8.0,
        },
        ModelOption {
            model: "qwen3.5:14b".into(),
            display_name: "Qwen 3.5 14B".into(),
            ram_required_gb: 12.0,
            suitable: ram_budget >= 12.0,
        },
        ModelOption {
            model: "qwen3.5:30b".into(),
            display_name: "Qwen 3.5 30B".into(),
            ram_required_gb: 24.0,
            suitable: ram_budget >= 24.0,
        },
        ModelOption {
            model: "qwen3.5:32b".into(),
            display_name: "Qwen 3.5 32B".into(),
            ram_required_gb: 26.0,
            suitable: ram_budget >= 26.0,
        },
    ];

    // Pick the largest model that fits
    let recommended = options
        .iter()
        .rev()
        .find(|o| o.suitable)
        .unwrap_or(&options[0]);

    let reason = format!(
        "{:.0}GB RAM detected, {:.0}GB budget → {}",
        info.total_memory_gb, ram_budget, recommended.display_name
    );

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
        assert_eq!(rec.model, "qwen3.5:1.5b");
    }

    #[test]
    fn test_recommend_model_high_ram() {
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
        assert_eq!(rec.model, "qwen3.5:32b");
    }

    #[test]
    fn test_recommend_model_16gb() {
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
        // 16 * 0.6 = 9.6 GB budget → 8B model (needs 8GB)
        assert_eq!(rec.model, "qwen3.5:8b");
    }
}
