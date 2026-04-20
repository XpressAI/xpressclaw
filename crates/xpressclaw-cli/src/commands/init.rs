use std::path::Path;

use xpressclaw_core::config::DEFAULT_CONFIG_TEMPLATE;

pub async fn run(path: &str) -> anyhow::Result<()> {
    let dir = Path::new(path);
    let config_path = dir.join("xpressclaw.yaml");

    if config_path.exists() {
        println!("xpressclaw.yaml already exists. Skipping.");
        return Ok(());
    }

    // Write default config
    std::fs::write(&config_path, DEFAULT_CONFIG_TEMPLATE)?;
    println!("Created xpressclaw.yaml");

    // Create data directory
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))?;
    let data_dir = Path::new(&home).join(".xpressclaw");
    std::fs::create_dir_all(&data_dir)?;
    println!("Created data directory: {}", data_dir.display());

    // ADR-023: Docker was removed. Harness images (pi, etc.) pull on
    // demand from GHCR when they're first referenced (task 10); nothing
    // to pre-pull at init time.

    println!();
    println!("xpressclaw initialized! Next steps:");
    println!("  1. Edit xpressclaw.yaml to configure your agents");
    println!("  2. Run `xpressclaw up` to start");
    println!();

    Ok(())
}
