use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;
use xpressclaw_core::config::Config;
use xpressclaw_core::db::Database;
use xpressclaw_core::projects::ProjectManager;

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Create .xpressclaw.yml without contacting the remote
    Init {
        /// Local Project name or exact canonical ID to share
        #[arg(long, visible_alias = "project-id", value_name = "NAME_OR_ID")]
        project: String,

        /// Git remote URL or absolute local repository path (never include credentials)
        #[arg(long)]
        remote: String,

        /// Branch in the synchronization repository
        #[arg(long, default_value = "main")]
        branch: String,

        /// Directory inside the synchronization repository
        #[arg(long)]
        store_path: Option<String>,

        /// Keep Project memory local instead of including it in the shared store
        #[arg(long)]
        no_project_memory: bool,

        /// Project repository where .xpressclaw.yml will be created
        #[arg(long, default_value = ".", value_name = "DIR")]
        project_dir: PathBuf,

        /// XpressClaw control-plane directory containing xpressclaw.yaml (Desktop: ~/.xpressclaw)
        #[arg(long, visible_alias = "workdir", value_name = "DIR")]
        control_plane_dir: Option<PathBuf>,
    },

    /// Fetch and non-destructively merge shared Project state
    Fetch {
        /// Acknowledge a first fetch into populated state or a two-sided merge
        #[arg(long)]
        force: bool,

        /// Project repository containing .xpressclaw.yml
        #[arg(long, default_value = ".", value_name = "DIR")]
        project_dir: PathBuf,

        /// XpressClaw control-plane directory containing xpressclaw.yaml (Desktop: ~/.xpressclaw)
        #[arg(long, visible_alias = "workdir", value_name = "DIR")]
        control_plane_dir: Option<PathBuf>,
    },

    /// Publish local portable Project state to the configured Git store
    Publish {
        /// Project repository containing .xpressclaw.yml
        #[arg(long, default_value = ".", value_name = "DIR")]
        project_dir: PathBuf,

        /// XpressClaw control-plane directory containing xpressclaw.yaml (Desktop: ~/.xpressclaw)
        #[arg(long, visible_alias = "workdir", value_name = "DIR")]
        control_plane_dir: Option<PathBuf>,
    },
}

pub async fn run(command: SyncCommand) -> Result<()> {
    match command {
        SyncCommand::Init {
            project,
            remote,
            branch,
            store_path,
            no_project_memory,
            project_dir,
            control_plane_dir,
        } => {
            let (_config, _, db) = open_local(control_plane_dir.as_deref(), &project_dir, false)?;
            let db = Arc::new(db);
            let selected_project = ProjectManager::new(db.clone()).resolve(&project)?;
            let store_path =
                store_path.unwrap_or_else(|| format!("projects/{}", selected_project.id));
            let outcome = xpressclaw_core::sync::initialize(
                &db,
                &project_dir,
                &selected_project.id,
                &remote,
                &branch,
                &store_path,
                !no_project_memory,
            )?;
            println!(
                "Selected Project {} ({}).",
                selected_project.name, selected_project.id
            );
            println!("Created {}", outcome.manifest_path.display());
            println!("No remote data was fetched or published.");
            println!("Run `xpressclaw sync publish` to create the shared snapshot.");
        }
        SyncCommand::Fetch {
            force,
            project_dir,
            control_plane_dir,
        } => {
            let (mut config, config_path, db) =
                open_local(control_plane_dir.as_deref(), &project_dir, true)?;
            let outcome =
                xpressclaw_core::sync::fetch(&db, &mut config, &config_path, &project_dir, force)?;
            print_outcome("Fetched", &outcome);
            println!("Restart xpressclaw before using the synchronized Project.");
        }
        SyncCommand::Publish {
            project_dir,
            control_plane_dir,
        } => {
            let (config, _, db) = open_local(control_plane_dir.as_deref(), &project_dir, false)?;
            let outcome = xpressclaw_core::sync::publish(&db, &config, &project_dir)?;
            print_outcome("Published", &outcome);
        }
    }
    Ok(())
}

fn open_local(
    control_plane_dir: Option<&Path>,
    project_dir: &Path,
    allow_missing_config: bool,
) -> Result<(Config, PathBuf, Database)> {
    let control_plane_dir =
        resolve_control_plane_dir(control_plane_dir, project_dir, allow_missing_config)?;
    let config_path = control_plane_dir.join("xpressclaw.yaml");
    let config = if config_path.is_file() {
        Config::load(&config_path).with_context(|| {
            format!(
                "failed to load control-plane config {}",
                config_path.display()
            )
        })?
    } else if allow_missing_config {
        Config::default()
    } else {
        unreachable!("control-plane discovery requires a config for this operation");
    };
    std::fs::create_dir_all(&config.system.data_dir).with_context(|| {
        format!(
            "failed to create local data directory {}",
            config.system.data_dir.display()
        )
    })?;
    let db = Database::open(&config.system.data_dir.join("xpressclaw.db"))?;
    Ok((config, config_path, db))
}

fn resolve_control_plane_dir(
    explicit: Option<&Path>,
    project_dir: &Path,
    allow_missing_config: bool,
) -> Result<PathBuf> {
    if let Some(explicit) = explicit {
        let directory = absolute_path(explicit)?;
        if !directory.exists() {
            anyhow::bail!(
                "control-plane directory {} does not exist. Pass `--control-plane-dir <DIR>` with the directory that contains xpressclaw.yaml.",
                directory.display()
            );
        }
        if !directory.is_dir() {
            anyhow::bail!(
                "control-plane path {} is not a directory. Pass `--control-plane-dir <DIR>` with the directory that contains xpressclaw.yaml.",
                directory.display()
            );
        }
        let directory = directory.canonicalize().with_context(|| {
            format!(
                "failed to resolve control-plane directory {}",
                directory.display()
            )
        })?;
        let config_path = directory.join("xpressclaw.yaml");
        if !config_path.is_file() && !allow_missing_config {
            anyhow::bail!(
                "no xpressclaw.yaml exists in control-plane directory {}. Desktop creates ~/.xpressclaw/xpressclaw.yaml after first-run setup. For CLI mode, pass the directory used with `xpressclaw up --workdir`, or run `xpressclaw init {}` to create a control plane here.",
                directory.display(),
                directory.display()
            );
        }
        return Ok(directory);
    }

    let project_dir = absolute_path(project_dir)?;
    if !project_dir.exists() {
        anyhow::bail!(
            "project repository directory {} does not exist. Pass `--project-dir <DIR>` with the repository being synchronized.",
            project_dir.display()
        );
    }
    if !project_dir.is_dir() {
        anyhow::bail!(
            "project repository path {} is not a directory. Pass `--project-dir <DIR>` with the repository being synchronized.",
            project_dir.display()
        );
    }
    let project_dir = project_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve project repository directory {}",
            project_dir.display()
        )
    })?;
    let candidates = discover_control_plane_dirs(&project_dir)?;
    select_discovered_control_plane(candidates, &project_dir, allow_missing_config)
}

fn select_discovered_control_plane(
    candidates: Vec<PathBuf>,
    project_dir: &Path,
    allow_missing_config: bool,
) -> Result<PathBuf> {
    match candidates.as_slice() {
        [directory] => Ok(directory.clone()),
        [] => {
            let fetch_hint = if allow_missing_config {
                " For a first fetch into a new control plane, pass the intended directory explicitly; xpressclaw.yaml will be created from the shared configuration."
            } else {
                ""
            };
            anyhow::bail!(
                "could not discover a control plane from project repository {}. No xpressclaw.yaml was found in the repository, its parent directories, a single sibling control-plane repository, or the Desktop default ~/.xpressclaw. If you use Desktop, launch it and finish first-run setup, then retry. For CLI mode, rerun with `--control-plane-dir /path/to/xpressclaw-control` (the directory used with `xpressclaw up --workdir`).{fetch_hint}",
                project_dir.display()
            );
        }
        _ => {
            let paths = candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "found multiple possible control-plane directories near {}: {paths}. Choose one explicitly with `--control-plane-dir <DIR>`.",
                project_dir.display()
            );
        }
    }
}

fn discover_control_plane_dirs(project_dir: &Path) -> Result<Vec<PathBuf>> {
    let current_dir = std::env::current_dir()
        .context("failed to resolve current directory")?
        .canonicalize()
        .context("failed to resolve current directory")?;
    let desktop_control_plane = default_desktop_control_plane_dir();
    discover_control_plane_dirs_from(project_dir, &current_dir, desktop_control_plane.as_deref())
}

fn discover_control_plane_dirs_from(
    project_dir: &Path,
    current_dir: &Path,
    desktop_control_plane: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    // Prefer an explicit context: when the command is run inside a control-plane
    // checkout, or the synchronized repository itself contains xpressclaw.yaml,
    // the nearest ancestor is unambiguous.
    for start in [current_dir, project_dir] {
        if let Some(directory) = start
            .ancestors()
            .find(|directory| directory.join("xpressclaw.yaml").is_file())
        {
            return Ok(vec![directory.to_path_buf()]);
        }
    }

    let mut candidates = BTreeSet::new();
    for start in [current_dir, project_dir] {
        let repository_root = start
            .ancestors()
            .find(|directory| directory.join(".git").exists())
            .unwrap_or(start);
        let Some(parent) = repository_root.parent() else {
            continue;
        };
        let entries = std::fs::read_dir(parent).with_context(|| {
            format!(
                "failed to inspect sibling directories next to {}",
                repository_root.display()
            )
        })?;
        for entry in entries {
            let path = entry?.path();
            if path.join("xpressclaw.yaml").is_file() {
                candidates.insert(path.canonicalize().with_context(|| {
                    format!("failed to resolve discovered directory {}", path.display())
                })?);
            }
        }
    }
    if !candidates.is_empty() {
        return Ok(candidates.into_iter().collect());
    }

    // The packaged Desktop app always starts its sidecar with ~/.xpressclaw as
    // the control-plane directory. Keep nearby CLI/source checkouts higher
    // priority, then make a Desktop-only installation work without an option.
    if let Some(directory) = desktop_control_plane {
        if directory.join("xpressclaw.yaml").is_file() {
            return Ok(vec![directory.canonicalize().with_context(|| {
                format!(
                    "failed to resolve Desktop control-plane directory {}",
                    directory.display()
                )
            })?]);
        }
    }

    Ok(Vec::new())
}

fn default_desktop_control_plane_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".xpressclaw"))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path))
    }
}

fn print_outcome(action: &str, outcome: &xpressclaw_core::sync::SyncOutcome) {
    let counts = outcome.counts;
    println!(
        "{action} Project {} at commit {}",
        outcome.project_id, outcome.commit
    );
    println!(
        "  {} agents, {} tasks, {} task messages, {} conversations, {} conversation messages, {} workflows, {} memory notes",
        counts.agents,
        counts.tasks,
        counts.task_messages,
        counts.conversations,
        counts.conversation_messages,
        counts.workflows,
        counts.memory_notes
    );
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::{CommandFactory, Parser};
    use tempfile::tempdir;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: SyncCommand,
    }

    #[test]
    fn init_keeps_legacy_project_id_and_workdir_aliases() {
        let parsed = TestCli::try_parse_from([
            "sync",
            "init",
            "--project-id",
            "project-uuid",
            "--remote",
            "file:///tmp/shared.git",
            "--workdir",
            ".",
        ])
        .unwrap();

        match parsed.command {
            SyncCommand::Init {
                project,
                control_plane_dir,
                ..
            } => {
                assert_eq!(project, "project-uuid");
                assert_eq!(control_plane_dir, Some(PathBuf::from(".")));
            }
            _ => panic!("expected sync init"),
        }
    }

    #[test]
    fn init_help_names_the_friendly_selector_and_control_plane() {
        let mut command = TestCli::command();
        let help = command
            .find_subcommand_mut("init")
            .unwrap()
            .render_long_help()
            .to_string();

        assert!(help.contains("--project <NAME_OR_ID>"));
        assert!(help.contains("--project-id"));
        assert!(help.contains("Local Project name or exact canonical ID"));
        assert!(help.contains("--control-plane-dir <DIR>"));
        assert!(help.contains("--workdir"));
        assert!(help.contains("containing xpressclaw.yaml"));
        assert!(help.contains("Desktop: ~/.xpressclaw"));
    }

    #[test]
    fn discovers_a_single_sibling_control_plane_from_a_project_repository() {
        let root = tempdir().unwrap();
        let project = root.path().join("platform");
        let control_plane = root.path().join("xpressclaw");
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::create_dir_all(&control_plane).unwrap();
        fs::write(control_plane.join("xpressclaw.yaml"), "system: {}\n").unwrap();

        let resolved = discover_control_plane_dirs_from(
            &project.canonicalize().unwrap(),
            &project.canonicalize().unwrap(),
            None,
        )
        .unwrap();

        assert_eq!(resolved, vec![control_plane.canonicalize().unwrap()]);
    }

    #[test]
    fn reports_wrong_and_missing_control_plane_directories_actionably() {
        let root = tempdir().unwrap();
        let missing = root.path().join("does-not-exist");
        let missing_error = resolve_control_plane_dir(Some(&missing), root.path(), false)
            .unwrap_err()
            .to_string();
        assert!(missing_error.contains("does not exist"));
        assert!(missing_error.contains("--control-plane-dir <DIR>"));

        let data_dir = root.path().join(".xpressclaw");
        fs::create_dir(&data_dir).unwrap();
        let wrong_error = resolve_control_plane_dir(Some(&data_dir), root.path(), false)
            .unwrap_err()
            .to_string();
        assert!(wrong_error.contains("no xpressclaw.yaml exists"));
        assert!(wrong_error.contains("xpressclaw init"));
        assert!(wrong_error.contains("Desktop creates ~/.xpressclaw/xpressclaw.yaml"));
        assert!(wrong_error.contains("xpressclaw up --workdir"));
        assert_eq!(
            resolve_control_plane_dir(Some(&data_dir), root.path(), true).unwrap(),
            data_dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn reports_when_no_control_plane_can_be_discovered() {
        let project = PathBuf::from("/work/platform");
        let error = select_discovered_control_plane(Vec::new(), &project, false)
            .unwrap_err()
            .to_string();

        assert!(error.contains("could not discover a control plane"));
        assert!(error.contains("/work/platform"));
        assert!(error.contains("No xpressclaw.yaml was found"));
        assert!(error.contains("--control-plane-dir /path/to/xpressclaw-control"));
        assert!(error.contains("Desktop default ~/.xpressclaw"));
        assert!(error.contains("finish first-run setup"));
    }

    #[test]
    fn reports_ambiguous_sibling_control_planes() {
        let root = tempdir().unwrap();
        let project = root.path().join("platform");
        fs::create_dir_all(project.join(".git")).unwrap();
        for name in ["control-one", "control-two"] {
            let directory = root.path().join(name);
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("xpressclaw.yaml"), "system: {}\n").unwrap();
        }

        let candidates = discover_control_plane_dirs_from(
            &project.canonicalize().unwrap(),
            &project.canonicalize().unwrap(),
            None,
        )
        .unwrap();
        let error = select_discovered_control_plane(candidates, &project, false)
            .unwrap_err()
            .to_string();

        assert!(error.contains("multiple possible control-plane directories"));
        assert!(error.contains("control-one"));
        assert!(error.contains("control-two"));
        assert!(error.contains("--control-plane-dir <DIR>"));
    }

    #[test]
    fn falls_back_to_the_desktop_control_plane_from_a_project_repository() {
        let root = tempdir().unwrap();
        let project = root.path().join("projects").join("platform");
        let desktop_control_plane = root.path().join("home").join(".xpressclaw");
        fs::create_dir_all(project.join(".git")).unwrap();
        fs::create_dir_all(&desktop_control_plane).unwrap();
        fs::write(
            desktop_control_plane.join("xpressclaw.yaml"),
            "system: {}\n",
        )
        .unwrap();

        let resolved = discover_control_plane_dirs_from(
            &project.canonicalize().unwrap(),
            &project.canonicalize().unwrap(),
            Some(&desktop_control_plane),
        )
        .unwrap();

        assert_eq!(
            resolved,
            vec![desktop_control_plane.canonicalize().unwrap()]
        );
    }
}
