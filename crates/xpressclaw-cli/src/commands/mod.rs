use clap::Subcommand;

mod budget;
mod c2w_smoke;
mod chat;
mod client;
mod down;
mod init;
mod logs;
mod memory;
mod pi_smoke;
mod rollback_smoke;
mod sop;
mod status;
mod tasks;
mod up;
mod write_bundled_wasm;

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

        /// Working directory (where xpressclaw.yaml lives)
        #[arg(short, long)]
        workdir: Option<String>,
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

    /// Smoke-test the c2w WASM runtime (ADR-023).
    ///
    /// Launches a minimal WASI hello-world guest via C2wHarness and
    /// verifies the lifecycle (launch → run → exit → stop) works. Exists
    /// while the pi harness (task 4) is under construction; removed once
    /// pi replaces it as the canonical smoke.
    C2wSmoke,

    /// Smoke-test the pi harness layer (ADR-023 task 4).
    ///
    /// Launches a noop WASI guest through PiHarness so the pi-specific
    /// defaults (per-agent workspace mount, env seeding for the LLM
    /// sidecar and xclaw socket) are exercised end-to-end. OCI pull
    /// isn't wired yet; the smoke uses a local WASM it generates
    /// on-the-fly. Removed once tasks 5/6 deliver the real pi launch
    /// path.
    PiSmoke,

    /// Smoke-test workspace snapshot/restore (ADR-023 task 8, MVP criterion 7).
    ///
    /// Launches a c2w guest, seeds a file, snapshots the workspace,
    /// simulates a rogue tool call by overwriting the workspace, then
    /// restores the snapshot and verifies the original state comes
    /// back. Removed once task 10 wires snapshot/restore into the
    /// real task dispatcher.
    RollbackSmoke,

    /// Write the bundled noop harness WASM to a file (ADR-023 task 10
    /// scaffolding). Used by `build.sh --push-pi-wasm` to produce a
    /// WASM artifact to push to a local registry without requiring
    /// wat2wasm on the host.
    WriteBundledWasm {
        /// Destination file path for the WASM module.
        path: String,
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
        Command::Up {
            detach,
            port,
            workdir,
        } => up::run(detach, port, workdir).await,
        Command::Down { port } => down::run(port).await,
        Command::Status { port } => status::run(port).await,
        Command::Chat { agent, port } => chat::run(&agent, port).await,
        Command::Tasks { command, port } => tasks::run(command, port).await,
        Command::Memory { command, port } => memory::run(command, port).await,
        Command::Budget { agent, port } => budget::run(agent, port).await,
        Command::Sop { command, port } => sop::run(command, port).await,
        Command::C2wSmoke => c2w_smoke::run().await,
        Command::PiSmoke => pi_smoke::run().await,
        Command::RollbackSmoke => rollback_smoke::run().await,
        Command::WriteBundledWasm { path } => write_bundled_wasm::run(path.into()).await,
        Command::Logs { agent, limit, port } => logs::run(agent, limit, port).await,
    }
}
