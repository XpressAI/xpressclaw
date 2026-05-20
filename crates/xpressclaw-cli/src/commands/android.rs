use std::path::{Path, PathBuf};
use std::process::Stdio;

use clap::Subcommand;
use tokio::process::Command;

const ANDROID_IMAGE_TAG: &str = "ghcr.io/xpressai/xpressclaw-harness-claude-sdk-android:latest";
const ANDROID_DOCKERFILE: &str = "harnesses/claude-sdk-android/Dockerfile";
const HARNESSES_DIR: &str = "harnesses";

#[derive(Subcommand)]
pub enum AndroidCommand {
    /// Install the Android-enabled harness image (adb + scrcpy)
    Install,

    /// Remove the Android-enabled harness image
    Uninstall,
}

pub async fn run(command: AndroidCommand) -> anyhow::Result<()> {
    match command {
        AndroidCommand::Install => install().await,
        AndroidCommand::Uninstall => uninstall().await,
    }
}

async fn install() -> anyhow::Result<()> {
    let repo_root = find_repo_root()?;
    let dockerfile = repo_root.join(ANDROID_DOCKERFILE);
    let build_context = repo_root.join(HARNESSES_DIR);

    if !dockerfile.exists() {
        anyhow::bail!(
            "Dockerfile not found at {}. Are you running from inside the xpressclaw repo?",
            dockerfile.display()
        );
    }

    println!("Building {ANDROID_IMAGE_TAG} from {}", dockerfile.display());
    println!("This pulls and adds adb + scrcpy v4.0 to the claude-sdk harness (~60 MB).");
    println!();

    let status = Command::new("docker")
        .arg("build")
        .arg("-t")
        .arg(ANDROID_IMAGE_TAG)
        .arg("-f")
        .arg(&dockerfile)
        .arg(&build_context)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("docker build failed (exit {})", status);
    }

    println!();
    println!("Done. Verify with:");
    println!("  docker run --rm {ANDROID_IMAGE_TAG} adb version");
    println!("  docker run --rm {ANDROID_IMAGE_TAG} scrcpy --version");
    Ok(())
}

async fn uninstall() -> anyhow::Result<()> {
    println!("Removing {ANDROID_IMAGE_TAG}");
    let status = Command::new("docker")
        .arg("rmi")
        .arg(ANDROID_IMAGE_TAG)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;
    if !status.success() {
        // Not an error if the image was already gone — docker rmi prints
        // its own diagnostic.
        eprintln!("(docker rmi exited {})", status);
    }
    Ok(())
}

/// Walk up from the current directory looking for the harnesses/ dir
/// (xpressclaw repo root marker).
fn find_repo_root() -> anyhow::Result<PathBuf> {
    let start = std::env::current_dir()?;
    let mut cur: &Path = &start;
    loop {
        if cur.join(HARNESSES_DIR).is_dir() && cur.join("Cargo.toml").is_file() {
            return Ok(cur.to_path_buf());
        }
        match cur.parent() {
            Some(p) => cur = p,
            None => anyhow::bail!(
                "Could not find xpressclaw repo root (looking for a parent dir with harnesses/ + Cargo.toml). \
                 Run this from inside the xpressclaw source tree."
            ),
        }
    }
}
