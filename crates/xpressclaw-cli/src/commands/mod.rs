use clap::Subcommand;

mod budget;
mod chat;
mod client;
mod down;
mod init;
mod logs;
mod memory;
mod sop;
mod status;
mod tasks;
mod up;

/// Default port for the xpressclaw server.
const DEFAULT_PORT: u16 = 8935;

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new xpressclaw workspace
    Init {
        /// Directory to initialize (default: current directory)
        #[arg(default_value = ".")]
        path: String,
    },

    /// Start the runtime and agents
    Up {
        /// Run in background (detached mode)
        #[arg(short, long)]
        detach: bool,

        /// Port for the web UI and API
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },

    /// Stop all agents
    Down {
        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },

    /// Show agent status and budget
    Status {
        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },

    /// Chat with an agent
    Chat {
        /// Agent name
        agent: String,

        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },

    /// Manage tasks
    Tasks {
        #[command(subcommand)]
        command: tasks::TasksCommand,

        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT, global = true)]
        port: u16,
    },

    /// Inspect and manage memory
    Memory {
        #[command(subcommand)]
        command: memory::MemoryCommand,

        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT, global = true)]
        port: u16,
    },

    /// Show budget and usage
    Budget {
        /// Filter by agent
        #[arg(short, long)]
        agent: Option<String>,

        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },

    /// Manage procedures (SOPs)
    Sop {
        #[command(subcommand)]
        command: sop::SopCommand,

        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT, global = true)]
        port: u16,
    },

    /// View activity logs
    Logs {
        /// Filter by agent
        #[arg(short, long)]
        agent: Option<String>,

        /// Number of entries
        #[arg(short = 'n', long, default_value = "50")]
        limit: usize,

        /// Server port
        #[arg(short, long, default_value_t = DEFAULT_PORT)]
        port: u16,
    },
}

pub async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Init { path } => init::run(&path).await,
        Command::Up { detach, port } => up::run(detach, port).await,
        Command::Down { port } => down::run(port).await,
        Command::Status { port } => status::run(port).await,
        Command::Chat { agent, port } => chat::run(&agent, port).await,
        Command::Tasks { command, port } => tasks::run(command, port).await,
        Command::Memory { command, port } => memory::run(command, port).await,
        Command::Budget { agent, port } => budget::run(agent, port).await,
        Command::Sop { command, port } => sop::run(command, port).await,
        Command::Logs {
            agent,
            limit,
            port,
        } => logs::run(agent, limit, port).await,
    }
}
