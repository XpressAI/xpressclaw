//! A private, short-lived terminal owned by one browser sign-in operation.
use super::*;

pub struct AuthenticationTerminal {
    pub terminal: TerminalSession,
    pub socket: String,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

impl DockerManager {
    async fn authentication_control(
        &self,
        agent: &str,
        command: Vec<String>,
        environment: &[String],
        cwd: Option<&str>,
    ) -> Result<String> {
        let name = self.owned_project_container_name(agent)?;
        let exec = self
            .docker
            .create_exec(
                &name,
                CreateExecOptions {
                    cmd: Some(command),
                    env: Some(environment.to_vec()),
                    working_dir: cwd.map(str::to_owned),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        let mut text = String::new();
        if let StartExecResults::Attached { mut output, .. } = self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|error| Error::Container(error.to_string()))?
        {
            while let Some(chunk) = output.next().await {
                text.push_str(
                    &chunk
                        .map_err(|error| Error::Container(error.to_string()))?
                        .to_string(),
                );
            }
        }
        let result = self
            .docker
            .inspect_exec(&exec.id)
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        if result.exit_code != Some(0) {
            return Err(Error::Container(format!(
                "Could not manage the sign-in terminal: {}",
                text.trim()
            )));
        }
        Ok(text)
    }

    pub async fn open_authentication_terminal(
        &self,
        agent: &str,
        command: &[String],
        environment: &[String],
        cwd: Option<&str>,
        columns: u16,
        rows: u16,
    ) -> Result<AuthenticationTerminal> {
        if command.is_empty() {
            return Err(Error::Container("No sign-in command is available".into()));
        }
        self.start_project_environment(agent).await?;
        let socket = format!("xpressclaw-auth-{}", uuid::Uuid::new_v4().simple());
        let tmux = |args: &[&str]| {
            let mut command = vec![
                "tmux".into(),
                "-L".into(),
                socket.clone(),
                "-f".into(),
                "/dev/null".into(),
            ];
            command.extend(args.iter().map(|arg| (*arg).to_owned()));
            command
        };
        let mut environment = environment.to_vec();
        environment.push("TERM=xterm-256color".into());
        let result = async {
            let launch = command
                .iter()
                .map(|arg| quote(arg))
                .collect::<Vec<_>>()
                .join(" ");
            // tmux 3.3 can lose pane_dead_status. Record only the command's exit
            // code on this private pane before the wrapper exits.
            let launch = format!(
                "{launch}; xpressclaw_auth_status=$?; tmux -L {} set-option -p -t sign-in @xpressclaw_auth_exit \"$xpressclaw_auth_status\"; exit \"$xpressclaw_auth_status\"",
                quote(&socket)
            );
            self.authentication_control(
                agent,
                tmux(&[
                    "start-server",
                    ";",
                    "set-option",
                    "-g",
                    "remain-on-exit",
                    "on",
                    ";",
                    "set-option",
                    "-g",
                    "status",
                    "off",
                    ";",
                    "new-session",
                    "-d",
                    "-s",
                    "sign-in",
                    "-x",
                    &columns.to_string(),
                    "-y",
                    &rows.to_string(),
                    &launch,
                ]),
                &environment,
                cwd,
            )
            .await?;
            let name = self.owned_project_container_name(agent)?;
            let exec = self
                .docker
                .create_exec(
                    &name,
                    CreateExecOptions {
                        cmd: Some(tmux(&["attach-session", "-t", "sign-in"])),
                        env: Some(environment),
                        tty: Some(true),
                        attach_stdin: Some(true),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| Error::Container(error.to_string()))?;
            let StartExecResults::Attached { output, input } = self
                .docker
                .start_exec(
                    &exec.id,
                    Some(StartExecOptions {
                        tty: true,
                        detach: false,
                        output_capacity: Some(256 * 1024),
                    }),
                )
                .await
                .map_err(|error| Error::Container(error.to_string()))?
            else {
                return Err(Error::Container(
                    "The sign-in terminal did not attach".into(),
                ));
            };
            let terminal = TerminalSession {
                exec_id: exec.id,
                input,
                output,
            };
            self.resize_terminal(&terminal.exec_id, columns, rows)
                .await?;
            Ok(terminal)
        }
        .await;
        match result {
            Ok(terminal) => Ok(AuthenticationTerminal { terminal, socket }),
            Err(error) => {
                self.close_authentication_terminal(agent, &socket).await;
                Err(error)
            }
        }
    }

    pub async fn authentication_terminal_exit(
        &self,
        agent: &str,
        socket: &str,
    ) -> Result<Option<i64>> {
        let result = self
            .authentication_control(
                agent,
                vec![
                    "tmux".into(),
                    "-L".into(),
                    socket.into(),
                    "display-message".into(),
                    "-p".into(),
                    "-t".into(),
                    "sign-in".into(),
                    "#{pane_dead}:#{@xpressclaw_auth_exit}".into(),
                ],
                &[],
                None,
            )
            .await?;
        let fields = result.trim().split(':').collect::<Vec<_>>();
        match fields.as_slice() {
            ["0", ""] => Ok(None),
            ["1", ""] => Ok(Some(-1)),
            ["0" | "1", code] => code
                .parse()
                .map(Some)
                .map_err(|_| Error::Container("Invalid sign-in process status".into())),
            _ => Err(Error::Container("Missing sign-in process status".into())),
        }
    }

    pub async fn close_authentication_terminal(&self, agent: &str, socket: &str) {
        // Only this operation's random private tmux server is terminated.
        let _ = self
            .authentication_control(
                agent,
                vec![
                    "tmux".into(),
                    "-L".into(),
                    socket.into(),
                    "kill-server".into(),
                ],
                &[],
                None,
            )
            .await;
    }
}
