use std::path::{Path, PathBuf};

use anyhow::Context;

pub const CONFIG_FILE: &str = "xpressclaw.yaml";
const INSTANCE_MARKER: &str = ".xpressclaw-instance";
const PID_FILE: &str = "server.pid";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceSource {
    Explicit,
    Default,
    LegacyCurrentDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    pub root: PathBuf,
    pub source: InstanceSource,
}

impl Instance {
    pub fn config_path(&self) -> PathBuf {
        self.root.join(CONFIG_FILE)
    }

    pub fn is_default(&self) -> bool {
        self.source == InstanceSource::Default
    }
}

pub fn default_root() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("could not determine the home directory for the default XpressClaw instance")?;
    Ok(PathBuf::from(home).join(".xpressclaw"))
}

pub fn resolve(explicit: Option<PathBuf>) -> anyhow::Result<Instance> {
    let current = std::env::current_dir().context("could not determine the current directory")?;
    let default = default_root()?;
    Ok(resolve_from(explicit, &current, &default))
}

pub fn mark_materialized(instance: &Instance) -> anyhow::Result<()> {
    std::fs::write(instance.root.join(INSTANCE_MARKER), "1\n").with_context(|| {
        format!(
            "failed to mark XpressClaw instance at {}",
            instance.root.display()
        )
    })
}

fn resolve_from(explicit: Option<PathBuf>, current: &Path, default: &Path) -> Instance {
    if let Some(root) = explicit {
        let root = if root.is_absolute() {
            root
        } else {
            current.join(root)
        };
        return Instance {
            root,
            source: InstanceSource::Explicit,
        };
    }

    if is_materialized(default) {
        return Instance {
            root: default.to_path_buf(),
            source: InstanceSource::Default,
        };
    }

    if current.join(CONFIG_FILE).is_file() && current != default {
        return Instance {
            root: current.to_path_buf(),
            source: InstanceSource::LegacyCurrentDirectory,
        };
    }

    Instance {
        root: default.to_path_buf(),
        source: InstanceSource::Default,
    }
}

fn is_materialized(root: &Path) -> bool {
    [CONFIG_FILE, INSTANCE_MARKER, PID_FILE]
        .into_iter()
        .any(|name| root.join(name).is_file())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn defaults_to_the_home_instance() {
        let root = tempdir().unwrap();
        let current = root.path().join("repo");
        let default = root.path().join("home-instance");
        std::fs::create_dir_all(&current).unwrap();

        let instance = resolve_from(None, &current, &default);

        assert_eq!(instance.root, default);
        assert_eq!(instance.source, InstanceSource::Default);
    }

    #[test]
    fn existing_default_instance_wins_over_a_repository_config() {
        let root = tempdir().unwrap();
        let current = root.path().join("repo");
        let default = root.path().join("home-instance");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&default).unwrap();
        std::fs::write(current.join(CONFIG_FILE), "agents: []\n").unwrap();
        std::fs::write(default.join(CONFIG_FILE), "agents: []\n").unwrap();

        let instance = resolve_from(None, &current, &default);

        assert_eq!(instance.root, default);
        assert_eq!(instance.source, InstanceSource::Default);
    }

    #[test]
    fn first_run_marker_marks_the_default_instance_before_config_exists() {
        let root = tempdir().unwrap();
        let current = root.path().join("repo");
        let default = root.path().join("home-instance");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&default).unwrap();
        std::fs::write(current.join(CONFIG_FILE), "agents: []\n").unwrap();
        mark_materialized(&Instance {
            root: default.clone(),
            source: InstanceSource::Default,
        })
        .unwrap();

        let instance = resolve_from(None, &current, &default);

        assert_eq!(instance.root, default);
        assert_eq!(instance.source, InstanceSource::Default);
    }

    #[test]
    fn legacy_shared_database_does_not_override_a_current_directory_config() {
        let root = tempdir().unwrap();
        let current = root.path().join("control-plane");
        let default = root.path().join("home-instance");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(&default).unwrap();
        std::fs::write(current.join(CONFIG_FILE), "agents: []\n").unwrap();
        std::fs::write(default.join("xpressclaw.db"), "legacy shared data").unwrap();

        let instance = resolve_from(None, &current, &default);

        assert_eq!(instance.root, current);
        assert_eq!(instance.source, InstanceSource::LegacyCurrentDirectory);
    }

    #[test]
    fn discovers_a_legacy_current_directory_config() {
        let root = tempdir().unwrap();
        let current = root.path().join("control-plane");
        let default = root.path().join("home-instance");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join(CONFIG_FILE), "agents: []\n").unwrap();

        let instance = resolve_from(None, &current, &default);

        assert_eq!(instance.root, current);
        assert_eq!(instance.source, InstanceSource::LegacyCurrentDirectory);
    }

    #[test]
    fn explicit_relative_instance_is_resolved_from_current_directory() {
        let root = tempdir().unwrap();
        let current = root.path().join("repo");
        let default = root.path().join("home-instance");

        let instance = resolve_from(Some(PathBuf::from("other")), &current, &default);

        assert_eq!(instance.root, current.join("other"));
        assert_eq!(instance.source, InstanceSource::Explicit);
    }
}
