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

    let src = workspace_root
        .join("target")
        .join(&profile)
        .join("xpressclaw");
    let dest = binaries_dir.join(format!("xpressclaw-{target_triple}"));

    if src.exists() {
        std::fs::copy(&src, &dest).ok();
    }

    tauri_build::build()
}
