use std::path::Path;

use xpressclaw_core::config::DEFAULT_CONFIG_TEMPLATE;
use xpressclaw_core::docker::manager::DockerManager;
use xpressclaw_core::docker::images;

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

    // Pull default harness image
    match DockerManager::connect().await {
        Ok(docker) => {
            println!("Pulling default harness image...");
            match images::pull_defaults(&docker).await {
                Ok(_) => println!("Harness image ready."),
                Err(e) => eprintln!("Warning: failed to pull harness image: {e}"),
            }
        }
        Err(_) => {
            eprintln!(
                "Warning: Docker/Podman not running. \
                 Harness images will be pulled when you run `xpressclaw up`."
            );
        }
    }

    println!();
    println!("xpressclaw initialized! Next steps:");
    println!("  1. Edit xpressclaw.yaml to configure your agents");
    println!("  2. Run `xpressclaw up` to start");
    println!();

    Ok(())
}
