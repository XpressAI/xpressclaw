use clap::Parser;
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
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    commands::run(cli.command).await
}
