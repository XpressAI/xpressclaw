use std::path::{Path, PathBuf};

use anyhow::Context;

use super::instance;

pub async fn run(path: Option<PathBuf>) -> anyhow::Result<()> {
    let default = instance::default_root()?;
    let explicit = path.is_some();
    let dir = path.unwrap_or_else(|| default.clone());
    let current = std::env::current_dir().context("could not determine the current directory")?;
    if !explicit && current != default {
        eprintln!(
            "note: init without a path now targets the default instance at {}; use `xpressclaw init .` for the legacy current-directory behavior",
            default.display()
        );
    }
    let dir = if dir.is_absolute() {
        dir
    } else {
        current.join(dir)
    };
    initialize(&dir, &default)
}

fn initialize(dir: &Path, default_instance: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create control-plane directory {}", dir.display()))?;
    let config_path = dir.join("xpressclaw.yaml");

    if config_path.exists() {
        println!("xpressclaw.yaml already exists. Skipping.");
        return Ok(());
    }

    let workspace_dir = dir.join("workspaces");
    std::fs::create_dir_all(&workspace_dir)?;
    std::fs::write(&config_path, initial_config(dir, &workspace_dir)?)?;

    println!("Initialized XpressClaw instance at {}", dir.display());
    println!("  Config: {}", config_path.display());
    println!("  Data:   {}", dir.display());

    println!();
    println!("Next steps:");
    if dir == default_instance {
        println!("  1. Run `xpressclaw up`");
    } else {
        println!("  1. Run `xpressclaw up --instance \"{}\"`", dir.display());
    }
    println!("  2. Open http://localhost:8935 and complete first-run setup");
    println!("  3. Add a repository and Agent in the web UI");
    println!();

    Ok(())
}

fn initial_config(data_dir: &Path, workspace_dir: &Path) -> anyhow::Result<String> {
    let data_dir = serde_json::to_string(&data_dir.display().to_string())?;
    let workspace_dir = serde_json::to_string(&workspace_dir.display().to_string())?;
    Ok(format!(
        r#"# XpressClaw instance configuration
# Repositories and Agents are added in the web UI; this file belongs to the
# control-plane instance, not to any one source repository.

system:
  isolation: docker
  data_dir: {data_dir}
  workspace_dir: {workspace_dir}

agents: []
"#
    ))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn creates_a_missing_control_plane_directory() {
        let root = tempdir().unwrap();
        let control_plane = root.path().join("control-plane");
        let default_instance = root.path().join("default");

        initialize(&control_plane, &default_instance).unwrap();

        assert!(control_plane.join("xpressclaw.yaml").is_file());
        assert!(control_plane.join("workspaces").is_dir());
        let config =
            xpressclaw_core::config::Config::load(&control_plane.join("xpressclaw.yaml")).unwrap();
        assert_eq!(config.system.data_dir, control_plane);
        assert_eq!(
            config.system.workspace_dir,
            control_plane.join("workspaces")
        );
    }
}
