use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = Path::new(&manifest_dir).parent().unwrap().parent().unwrap();
    let frontend_dir = workspace_root.join("frontend");
    let build_dir = frontend_dir.join("build");

    // Tell Cargo to rerun this script if any frontend source changes
    println!("cargo:rerun-if-changed={}", frontend_dir.join("src").display());
    println!("cargo:rerun-if-changed={}", frontend_dir.join("static").display());
    println!("cargo:rerun-if-changed={}", frontend_dir.join("svelte.config.js").display());
    println!("cargo:rerun-if-changed={}", frontend_dir.join("vite.config.ts").display());
    println!("cargo:rerun-if-changed={}", frontend_dir.join("package.json").display());

    // Skip frontend build if already built (e.g. CI pre-builds it)
    if build_dir.join("index.html").exists() {
        return;
    }

    // Install deps if needed
    if !frontend_dir.join("node_modules").exists() {
        println!("cargo:warning=Installing frontend dependencies...");
        let status = Command::new("npm")
            .args(["ci", "--silent"])
            .current_dir(&frontend_dir)
            .status();
        if let Err(e) = status {
            println!("cargo:warning=npm ci failed: {e} (frontend will not be embedded)");
            return;
        }
    }

    // Build frontend
    println!("cargo:warning=Building frontend...");
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(&frontend_dir)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!("cargo:warning=Frontend built successfully");
        }
        Ok(s) => {
            println!("cargo:warning=Frontend build failed with status {s}");
        }
        Err(e) => {
            println!("cargo:warning=Failed to run npm: {e}");
        }
    }
}
