fn main() {
    // Copy the xpressclaw CLI binary for sidecar usage during development.
    // In production, `tauri build` handles this automatically.
    // In Bazel, the sidecar is declared as `data` dependency.
    let target_triple = std::env::var("TARGET").unwrap_or_else(|_| {
        std::process::Command::new("rustc")
            .args(["--print", "host-tuple"])
            .output()
            .map(|o| {
                String::from_utf8(o.stdout)
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            })
            .unwrap_or_else(|_| "x86_64-apple-darwin".to_string())
    });

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let binaries_dir = std::path::Path::new(&manifest_dir).join("binaries");
    let _ = std::fs::create_dir_all(&binaries_dir);

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    let exe_suffix = if target_triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let dest = binaries_dir.join(format!("xpressclaw-{target_triple}{exe_suffix}"));

    // Try to find and copy the CLI binary (skip silently in Bazel)
    if let Some(workspace_root) = std::path::Path::new(&manifest_dir)
        .parent()
        .and_then(|p| p.parent())
    {
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

        let copied = candidates
            .iter()
            .any(|src| src.exists() && std::fs::copy(src, &dest).is_ok());

        if !copied && !dest.exists() {
            let _ = std::fs::write(&dest, "placeholder");
        }
    } else if !dest.exists() {
        let _ = std::fs::write(&dest, "placeholder");
    }

    tauri_build::build()
}
