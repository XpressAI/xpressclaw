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

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();
    let frontend_dir = workspace_root.join("frontend");
    let build_marker = frontend_dir.join("build").join("index.html");

    // Tell Cargo to rerun this script if any frontend source file changes.
    // We must walk the directory tree and emit each file individually because
    // rerun-if-changed on a directory only fires when direct children are
    // added/removed — not when nested files are modified.
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

    // Skip rebuild if the build output is newer than all source files.
    if let Ok(build_mtime) = build_marker.metadata().and_then(|m| m.modified()) {
        if build_mtime >= newest_source {
            return;
        }
    }

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

    // Build frontend
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
