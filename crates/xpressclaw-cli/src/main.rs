use clap::Parser;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

mod commands;

#[derive(Parser)]
#[command(
    name = "xpressclaw",
    about = "Control plane for native agent work",
    version,
    long_about = "Run the XpressClaw control plane for durable Agents, queued work, \
                  schedules, and isolated Codex, Claude Code, DeepSeek Harness, OpenCode, or other ACP workers. \
                  Create Projects and operate Agents from the web UI."
)]
struct Cli {
    #[command(subcommand)]
    command: commands::Command,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::filter::filter_fn(
            xpressclaw_core::workers::acp::allow_log_metadata,
        ))
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    commands::run(cli.command).await
}
