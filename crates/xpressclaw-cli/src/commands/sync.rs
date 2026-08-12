use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use xpressclaw_core::config::Config;
use xpressclaw_core::db::Database;

#[derive(Subcommand)]
pub enum SyncCommand {
    /// Create .xpressclaw.yml without contacting the remote
    Init {
        /// Local XpressClaw Project ID to share
        #[arg(long)]
        project_id: String,

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

        /// Main project directory where .xpressclaw.yml is preserved
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Control-plane directory containing xpressclaw.yaml
        #[arg(long)]
        workdir: Option<PathBuf>,
    },

    /// Fetch and non-destructively merge shared Project state
    Fetch {
        /// Acknowledge a first fetch into populated state or a two-sided merge
        #[arg(long)]
        force: bool,

        /// Main project directory containing .xpressclaw.yml
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Control-plane directory containing xpressclaw.yaml
        #[arg(long)]
        workdir: Option<PathBuf>,
    },

    /// Publish local portable Project state to the configured Git store
    Publish {
        /// Main project directory containing .xpressclaw.yml
        #[arg(long, default_value = ".")]
        project_dir: PathBuf,

        /// Control-plane directory containing xpressclaw.yaml
        #[arg(long)]
        workdir: Option<PathBuf>,
    },
}

pub async fn run(command: SyncCommand) -> Result<()> {
    match command {
        SyncCommand::Init {
            project_id,
            remote,
            branch,
            store_path,
            no_project_memory,
            project_dir,
            workdir,
        } => {
            let (_config, _, db) = open_local(workdir.as_deref(), false)?;
            let store_path = store_path.unwrap_or_else(|| format!("projects/{project_id}"));
            let outcome = xpressclaw_core::sync::initialize(
                &db,
                &project_dir,
                &project_id,
                &remote,
                &branch,
                &store_path,
                !no_project_memory,
            )?;
            println!("Created {}", outcome.manifest_path.display());
            println!("No remote data was fetched or published.");
            println!("Run `xpressclaw sync publish` to create the shared snapshot.");
        }
        SyncCommand::Fetch {
            force,
            project_dir,
            workdir,
        } => {
            let (mut config, config_path, db) = open_local(workdir.as_deref(), true)?;
            let outcome =
                xpressclaw_core::sync::fetch(&db, &mut config, &config_path, &project_dir, force)?;
            print_outcome("Fetched", &outcome);
            println!("Restart xpressclaw before using the synchronized Project.");
        }
        SyncCommand::Publish {
            project_dir,
            workdir,
        } => {
            let (config, _, db) = open_local(workdir.as_deref(), false)?;
            let outcome = xpressclaw_core::sync::publish(&db, &config, &project_dir)?;
            print_outcome("Published", &outcome);
        }
    }
    Ok(())
}

fn open_local(
    workdir: Option<&Path>,
    allow_missing_config: bool,
) -> Result<(Config, PathBuf, Database)> {
    let workdir = workdir
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_dir().context("failed to resolve current directory")?);
    if !workdir.is_dir() {
        anyhow::bail!(
            "control-plane directory {} does not exist",
            workdir.display()
        );
    }
    let config_path = workdir.join("xpressclaw.yaml");
    let config = if config_path.exists() {
        Config::load(&config_path)?
    } else if allow_missing_config {
        Config::default()
    } else {
        anyhow::bail!(
            "{} does not exist; pass --workdir for the control-plane directory",
            config_path.display()
        );
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
