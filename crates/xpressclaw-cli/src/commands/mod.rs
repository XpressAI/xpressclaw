use clap::Subcommand;

mod client;
mod down;
mod init;
mod status;
mod up;

/// Default port for the xpressclaw server.
const DEFAULT_PORT: u16 = 8935;

#[derive(Subcommand)]
pub enum Command {
    /// Create an empty control-plane configuration
    Init {
        /// Directory to initialize (default: current directory)
        #[arg(default_value = ".")]
        path: String,
    },

    /// Start the control plane and web UI
    Up {
        /// Run in background (detached mode)
        #[arg(short, long)]
        detach: bool,

        /// Port for the web UI and API
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,

        /// Working directory (where xpressclaw.yaml lives)
        #[arg(short, long)]
        workdir: Option<String>,
    },

    /// Stop the control plane and active workers
    Down {
        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },

    /// Show control-plane and session status
    Status {
        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
}

pub async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Init { path } => init::run(&path).await,
        Command::Up {
            detach,
            port,
            workdir,
        } => up::run(detach, port, workdir).await,
        Command::Down { port } => down::run(port).await,
        Command::Status { port } => status::run(port).await,
    }
}
