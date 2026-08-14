use std::path::Path;

use anyhow::Context;
use xpressclaw_core::config::DEFAULT_CONFIG_TEMPLATE;

pub async fn run(path: &str) -> anyhow::Result<()> {
    let dir = Path::new(path);
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))?;
    initialize(dir, &Path::new(&home).join(".xpressclaw"))
}

fn initialize(dir: &Path, data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create control-plane directory {}", dir.display()))?;
    let config_path = dir.join("xpressclaw.yaml");

    if config_path.exists() {
        println!("xpressclaw.yaml already exists. Skipping.");
        return Ok(());
    }

    // Write default config
    std::fs::write(&config_path, DEFAULT_CONFIG_TEMPLATE)?;
    println!("Created xpressclaw.yaml");

    // Create data directory
    std::fs::create_dir_all(data_dir)?;
    println!("Created data directory: {}", data_dir.display());

    println!();
    println!("xpressclaw initialized! Next steps:");
    println!("  1. Run `xpressclaw up --workdir \"{}\"`", dir.display());
    println!("  2. Open http://localhost:8935 and complete first-run setup");
    println!("Runner images are selected and prepared per Agent.");
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn creates_a_missing_control_plane_directory() {
        let root = tempdir().unwrap();
        let control_plane = root.path().join("control-plane");
        let data_dir = root.path().join("data");

        initialize(&control_plane, &data_dir).unwrap();

        assert!(control_plane.join("xpressclaw.yaml").is_file());
        assert!(data_dir.is_dir());
    }
}
