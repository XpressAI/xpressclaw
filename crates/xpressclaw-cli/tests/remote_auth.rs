#![cfg(unix)]

use std::io::{Read, Write};
use std::process::Command;
use std::time::{Duration, Instant};

use xpressclaw_core::config::Config;

struct DetachedGuard(std::path::PathBuf);

impl Drop for DetachedGuard {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.0.join("server.pid")) {
            let _ = Command::new("kill").arg(pid.trim()).output();
        }
    }
}

fn unused_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn wait_for_listener(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
}

fn http_status(port: u16, path: &str) -> u16 {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .expect("HTTP response status")
}

#[test]
fn detached_launcher_prints_token_without_writing_it_to_server_log() {
    let root = tempfile::tempdir().unwrap();
    let instance = root.path().join("instance");
    std::fs::create_dir_all(&instance).unwrap();
    let mut config = Config::default();
    config.system.data_dir = instance.clone();
    config.system.workspace_dir = instance.join("workspaces");
    config.instance.authentication_enabled = true;
    config.save(&instance.join("xpressclaw.yaml")).unwrap();

    let port = unused_port();
    let binary = env!("CARGO_BIN_EXE_xpressclaw");
    let output = Command::new(binary)
        .args([
            "up",
            "--detach",
            "--instance",
            instance.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _guard = DetachedGuard(instance.clone());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let token = stdout
        .lines()
        .find_map(|line| line.strip_prefix("XPRESSCLAW_STARTUP_TOKEN="))
        .expect("detached launcher should print the startup token")
        .to_string();
    assert!(token.len() >= 40);

    let log_path = instance.join("server.log");
    wait_for_listener(port);
    let log = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(!log.contains(&token));
    assert!(!log.contains("XPRESSCLAW_STARTUP_TOKEN="));

    let stopped = Command::new(binary)
        .args(["down", "--instance", instance.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(stopped.status.success());
}

#[test]
fn confirmed_saved_wildcard_no_auth_starts_without_repeating_the_cli_warning() {
    let root = tempfile::tempdir().unwrap();
    let instance = root.path().join("tailnet-instance");
    std::fs::create_dir_all(&instance).unwrap();
    let mut config = Config::default();
    config.system.data_dir = instance.clone();
    config.system.workspace_dir = instance.join("workspaces");
    config.instance.bind = "0.0.0.0".parse().unwrap();
    config.instance.port = unused_port();
    config.instance.allow_unauthenticated_remote = true;
    config.save(&instance.join("xpressclaw.yaml")).unwrap();

    let binary = env!("CARGO_BIN_EXE_xpressclaw");
    let output = Command::new(binary)
        .args(["up", "--detach", "--instance", instance.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _guard = DetachedGuard(instance.clone());
    wait_for_listener(config.instance.port);

    assert_ne!(http_status(config.instance.port, "/api/projects/"), 401);

    let stopped = Command::new(binary)
        .args(["down", "--instance", instance.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(stopped.status.success());
}
