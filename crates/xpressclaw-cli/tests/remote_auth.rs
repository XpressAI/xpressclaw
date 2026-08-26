#![cfg(unix)]

use std::io::{Read, Write};
use std::process::{Command, Stdio};
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

fn http_json(port: u16, path: &str) -> serde_json::Value {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
}

fn http_post_json(port: u16, path: &str, body: &str) -> serde_json::Value {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    serde_json::from_str(body).unwrap()
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
fn detached_password_mode_does_not_print_an_unused_candidate_token() {
    let root = tempfile::tempdir().unwrap();
    let instance = root.path().join("password-instance");
    std::fs::create_dir_all(&instance).unwrap();
    let mut config = Config::default();
    config.system.data_dir = instance.clone();
    config.system.workspace_dir = instance.join("workspaces");
    config.instance.authentication_enabled = true;
    config.save(&instance.join("xpressclaw.yaml")).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let hash = runtime
        .block_on(xpressclaw_server::auth::hash_password(
            zeroize::Zeroizing::new("configured password value".to_string()),
        ))
        .unwrap();
    xpressclaw_server::auth::store_password_hash(&instance, Some(&hash)).unwrap();

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
    assert!(!String::from_utf8_lossy(&output.stdout).contains("XPRESSCLAW_STARTUP_TOKEN="));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("XPRESSCLAW_STARTUP_TOKEN="));
    wait_for_listener(port);
    let log = std::fs::read_to_string(instance.join("server.log")).unwrap_or_default();
    assert!(!log.contains("XPRESSCLAW_STARTUP_TOKEN="));

    let stopped = Command::new(binary)
        .args(["down", "--instance", instance.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(stopped.status.success());
}

#[test]
fn foreground_does_not_announce_a_token_when_the_listener_is_owned() {
    let root = tempfile::tempdir().unwrap();
    let instance = root.path().join("occupied-instance");
    std::fs::create_dir_all(&instance).unwrap();
    let mut config = Config::default();
    config.system.data_dir = instance.clone();
    config.system.workspace_dir = instance.join("workspaces");
    config.instance.authentication_enabled = true;
    config.save(&instance.join("xpressclaw.yaml")).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let output = Command::new(env!("CARGO_BIN_EXE_xpressclaw"))
        .env("XPRESSCLAW_DESKTOP_HANDSHAKE", "1")
        .args([
            "up",
            "--instance",
            instance.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("XPRESSCLAW_STARTUP_TOKEN="));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("XPRESSCLAW_STARTUP_TOKEN="));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("XPRESSCLAW_INSTANCE_IDENTITY="));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("XPRESSCLAW_INSTANCE_IDENTITY="));
}

#[test]
fn foreground_announces_desktop_identity_only_after_listener_ownership() {
    use std::io::BufRead;
    use std::sync::mpsc;

    let root = tempfile::tempdir().unwrap();
    let instance = root.path().join("desktop-handshake-instance");
    std::fs::create_dir_all(&instance).unwrap();
    let mut config = Config::default();
    config.system.data_dir = instance.clone();
    config.system.workspace_dir = instance.join("workspaces");
    config.save(&instance.join("xpressclaw.yaml")).unwrap();

    let port = unused_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_xpressclaw"))
        .env("XPRESSCLAW_DESKTOP_HANDSHAKE", "1")
        .args([
            "up",
            "--instance",
            instance.to_str().unwrap(),
            "--port",
            &port.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            if let Some(identity) = line.strip_prefix("XPRESSCLAW_INSTANCE_IDENTITY=") {
                let _ = sender.send(identity.to_string());
                return;
            }
        }
    });

    let identity = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the owned foreground listeners should announce their identity");
    assert!(identity.len() >= 40);
    assert!(identity
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
    assert_eq!(http_status(port, "/api/health"), 200);
    let bootstrap = http_json(port, "/api/auth/bootstrap");
    assert_eq!(bootstrap["identity_public_key"], identity);
    let proof = http_post_json(
        port,
        "/api/auth/identity-proof",
        r#"{"challenge":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#,
    );
    assert_eq!(proof["instance_id"], bootstrap["instance_id"]);
    assert_eq!(
        proof["identity_public_key"],
        bootstrap["identity_public_key"]
    );
    assert!(proof["signature"].as_str().unwrap().len() >= 80);

    child.kill().unwrap();
    child.wait().unwrap();
    reader.join().unwrap();
}

#[test]
fn detached_does_not_announce_a_token_or_succeed_before_listener_ownership() {
    let root = tempfile::tempdir().unwrap();
    let instance = root.path().join("occupied-detached-instance");
    std::fs::create_dir_all(&instance).unwrap();
    let mut config = Config::default();
    config.system.data_dir = instance.clone();
    config.system.workspace_dir = instance.join("workspaces");
    config.instance.authentication_enabled = true;
    config.save(&instance.join("xpressclaw.yaml")).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let output = Command::new(env!("CARGO_BIN_EXE_xpressclaw"))
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

    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("XPRESSCLAW_STARTUP_TOKEN="));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("XPRESSCLAW_STARTUP_TOKEN="));
    assert!(!instance.join("server.pid").exists());
    let log = std::fs::read_to_string(instance.join("server.log")).unwrap_or_default();
    assert!(!log.contains("XPRESSCLAW_STARTUP_TOKEN="));
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
