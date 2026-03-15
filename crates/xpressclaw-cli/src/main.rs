use clap::Parser;
use tracing_subscriber::EnvFilter;

mod commands;

#[derive(Parser)]
#[command(
    name = "xpressclaw",
    about = "Your AI agents. Running while you sleep.",
    version,
    long_about = "xpressclaw is an open-source AI agent runtime. Define agents in YAML, \
                  give them tasks, and let them work autonomously."
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
