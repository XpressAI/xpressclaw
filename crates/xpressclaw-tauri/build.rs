fn main() {
    // Copy the xpressclaw CLI binary for sidecar usage during development.
    // In production, `tauri build` handles this automatically.
    let target_triple = std::env::var("TARGET").unwrap_or_else(|_| {
        let output = std::process::Command::new("rustc")
            .args(["--print", "host-tuple"])
            .output()
            .expect("failed to run rustc");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    });

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let binaries_dir = std::path::Path::new(&manifest_dir).join("binaries");
    std::fs::create_dir_all(&binaries_dir).ok();

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // Windows binaries have .exe extension
    let exe_suffix = if target_triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let dest = binaries_dir.join(format!("xpressclaw-{target_triple}{exe_suffix}"));

    // Check multiple possible locations for the CLI binary:
    // 1. target/{profile}/xpressclaw (native build)
    // 2. target/{triple}/{profile}/xpressclaw (cross-compilation / cargo tauri build)
    let candidates = [
        workspace_root
            .join("target")
            .join(&profile)
            .join(format!("xpressclaw{exe_suffix}")),
        workspace_root
            .join("target")
            .join(&target_triple)
            .join(&profile)
            .join(format!("xpressclaw{exe_suffix}")),
    ];

    let copied = candidates.iter().any(|src| {
        if src.exists() {
            std::fs::copy(src, &dest).is_ok()
        } else {
            false
        }
    });

    if !copied && !dest.exists() {
        // Create a placeholder so tauri_build doesn't fail during CI/clippy.
        // The real binary must be built before running the app.
        std::fs::write(&dest, "placeholder").ok();
    }

    tauri_build::build()
}
