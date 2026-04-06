use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

/// Returns the npm command name for the current platform.
/// Windows needs "npm.cmd" because npm is a batch script.
fn npm() -> &'static str {
    if cfg!(target_os = "windows") {
        "npm.cmd"
    } else {
        "npm"
    }
}

/// Recursively emit `cargo:rerun-if-changed` for every file in a directory
/// and return the most recent mtime found.
fn walk_dir(dir: &Path, newest: &mut SystemTime) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_dir(&path, newest);
            } else {
                println!("cargo:rerun-if-changed={}", path.display());
                if let Ok(meta) = path.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        if mtime > *newest {
                            *newest = mtime;
                        }
                    }
                }
            }
        }
    }
}

/// Hash all files in a directory to detect content changes.
fn hash_dir(dir: &Path, hasher: &mut DefaultHasher) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort(); // deterministic order
        for path in paths {
            if path.is_dir() {
                hash_dir(&path, hasher);
            } else {
                path.display().to_string().hash(hasher);
                if let Ok(meta) = path.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        mtime.hash(hasher);
                    }
                    meta.len().hash(hasher);
                }
            }
        }
    }
}

fn main() {
    // Embed git commit hash at compile time
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        if output.status.success() {
            let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo:rustc-env=XPRESSCLAW_GIT_HASH={hash}");
        }
    }
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();
    let frontend_dir = workspace_root.join("frontend");
    let build_dir = frontend_dir.join("build");
    let build_marker = build_dir.join("index.html");

    // Also watch the build output — if something external (e.g. `npm run build`
    // or Tauri's beforeBuildCommand) rebuilds the frontend, we need to re-hash.
    if build_dir.is_dir() {
        println!("cargo:rerun-if-changed={}", build_dir.display());
    }

    // Tell Cargo to rerun this script if any frontend source file changes.
    let mut newest_source = SystemTime::UNIX_EPOCH;
    for dir in &["src", "static"] {
        let target = frontend_dir.join(dir);
        if target.is_dir() {
            walk_dir(&target, &mut newest_source);
        }
    }
    for file in &[
        "svelte.config.js",
        "vite.config.ts",
        "package.json",
        "tsconfig.json",
        "tailwind.config.ts",
    ] {
        let p = frontend_dir.join(file);
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
            if let Ok(mtime) = p.metadata().and_then(|m| m.modified()) {
                if mtime > newest_source {
                    newest_source = mtime;
                }
            }
        }
    }

    // Rebuild frontend if source files are newer than build output.
    let needs_build = match build_marker.metadata().and_then(|m| m.modified()) {
        Ok(build_mtime) => build_mtime < newest_source,
        Err(_) => true, // no build output yet
    };

    if needs_build {
        // Install deps if needed
        if !frontend_dir.join("node_modules").exists() {
            println!("cargo:warning=Installing frontend dependencies...");
            let status = Command::new(npm())
                .args(["ci", "--silent"])
                .current_dir(&frontend_dir)
                .status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => panic!("npm ci failed with status {s}"),
                Err(e) => panic!("npm ci failed: {e} — is Node.js installed?"),
            }
        }

        println!("cargo:warning=Building frontend...");
        let status = Command::new(npm())
            .args(["run", "build"])
            .current_dir(&frontend_dir)
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("cargo:warning=Frontend built successfully");
            }
            Ok(s) => panic!("Frontend build failed with status {s}"),
            Err(e) => panic!("Failed to run npm: {e} — is Node.js installed?"),
        }
    }

    // Hash the build output so cargo knows to recompile when embedded
    // assets change. Without this, rust-embed's proc macro won't re-run
    // because cargo sees no Rust source changes.
    let mut hasher = DefaultHasher::new();
    if build_dir.is_dir() {
        hash_dir(&build_dir, &mut hasher);
    }
    println!("cargo:rustc-env=FRONTEND_HASH={:016x}", hasher.finish());
}
