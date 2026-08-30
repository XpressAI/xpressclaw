use std::net::IpAddr;
use std::path::PathBuf;

use clap::Subcommand;

mod client;
mod down;
mod init;
mod instance;
mod status;
mod sync;
mod up;

#[derive(Subcommand)]
pub enum Command {
    /// Initialize an advanced control-plane instance (optional)
    Init {
        /// Instance directory (default: ~/.xpressclaw; use . for legacy current-directory behavior)
        #[arg(value_name = "INSTANCE")]
        path: Option<PathBuf>,
    },

    /// Start the control plane and web UI
    Up {
        /// Run in background (detached mode)
        #[arg(short, long)]
        detach: bool,

        /// Port for the web UI and API
        #[arg(short, long)]
        port: Option<u16>,

        /// Advanced: instance directory containing xpressclaw.yaml
        #[arg(long, value_name = "DIR", conflicts_with = "workdir")]
        instance: Option<PathBuf>,

        /// Deprecated alias for --instance
        #[arg(short, long, value_name = "DIR", conflicts_with = "instance")]
        workdir: Option<PathBuf>,

        /// Address on which the control plane listens
        #[arg(long)]
        bind: Option<IpAddr>,

        /// Acknowledge direct non-loopback access without app authentication
        #[arg(long)]
        allow_insecure_remote: bool,

        /// Read the detached-launcher startup handshake from stdin
        #[arg(long, hide = true)]
        startup_token_stdin: bool,
    },

    /// Stop the control plane and active workers
    Down {
        /// Server port
        #[arg(short, long)]
        port: Option<u16>,

        /// Advanced: instance directory containing the detached process PID
        #[arg(long, value_name = "DIR", conflicts_with = "workdir")]
        instance: Option<PathBuf>,

        /// Deprecated alias for --instance
        #[arg(short, long, value_name = "DIR", conflicts_with = "instance")]
        workdir: Option<PathBuf>,
    },

    /// Show control-plane and Agent status
    Status {
        /// Server port (default: saved instance port or 8935)
        #[arg(short, long)]
        port: Option<u16>,

        /// Advanced: instance directory containing xpressclaw.yaml
        #[arg(long, value_name = "DIR")]
        instance: Option<PathBuf>,
    },

    /// Explicitly synchronize portable Project state through Git
    Sync {
        #[command(subcommand)]
        command: sync::SyncCommand,
    },
}

pub async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Init { path } => init::run(path).await,
        Command::Up {
            detach,
            port,
            instance,
            workdir,
            bind,
            allow_insecure_remote,
            startup_token_stdin,
        } => {
            up::run(
                detach,
                port,
                instance,
                workdir,
                bind,
                allow_insecure_remote,
                startup_token_stdin,
            )
            .await
        }
        Command::Down {
            port,
            instance,
            workdir,
        } => down::run(port, instance, workdir).await,
        Command::Status { port, instance } => status::run(port, instance).await,
        Command::Sync { command } => sync::run(command).await,
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Command,
    }

    #[test]
    fn up_defaults_to_a_loopback_discovered_instance() {
        let parsed = TestCli::try_parse_from(["xpressclaw", "up"]).unwrap();
        let Command::Up {
            instance,
            workdir,
            bind,
            allow_insecure_remote,
            ..
        } = parsed.command
        else {
            panic!("expected up command");
        };

        assert!(instance.is_none());
        assert!(workdir.is_none());
        assert!(bind.is_none());
        assert!(!allow_insecure_remote);
    }

    #[test]
    fn up_keeps_the_workdir_compatibility_flag() {
        let parsed =
            TestCli::try_parse_from(["xpressclaw", "up", "--workdir", "/tmp/legacy-control-plane"])
                .unwrap();
        let Command::Up {
            instance, workdir, ..
        } = parsed.command
        else {
            panic!("expected up command");
        };

        assert!(instance.is_none());
        assert_eq!(workdir, Some(PathBuf::from("/tmp/legacy-control-plane")));
    }

    #[test]
    fn down_keeps_the_workdir_compatibility_flag() {
        let parsed = TestCli::try_parse_from([
            "xpressclaw",
            "down",
            "--workdir",
            "/tmp/legacy-control-plane",
        ])
        .unwrap();
        let Command::Down {
            instance, workdir, ..
        } = parsed.command
        else {
            panic!("expected down command");
        };

        assert!(instance.is_none());
        assert_eq!(workdir, Some(PathBuf::from("/tmp/legacy-control-plane")));
    }

    #[test]
    fn up_help_distinguishes_instances_and_remote_risk() {
        let mut command = TestCli::command();
        let up = command.find_subcommand_mut("up").unwrap();
        let mut help = Vec::new();
        up.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();

        assert!(help.contains("--instance <DIR>"));
        assert!(help.contains("Deprecated alias for --instance"));
        assert!(help.contains("--bind <BIND>"));
        assert!(help.contains("without app authentication"));
    }

    #[test]
    fn init_path_is_optional_and_named_as_an_instance() {
        let parsed = TestCli::try_parse_from(["xpressclaw", "init"]).unwrap();
        assert!(matches!(parsed.command, Command::Init { path: None }));

        let mut command = TestCli::command();
        let init = command.find_subcommand_mut("init").unwrap();
        let mut help = Vec::new();
        init.write_long_help(&mut help).unwrap();
        let help = String::from_utf8(help).unwrap();
        assert!(help.contains("[INSTANCE]"));
        assert!(help.contains("default: ~/.xpressclaw"));
        assert!(help.contains("legacy current-directory behavior"));
    }
}
