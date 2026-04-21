//! `xpressclaw write-bundled-wasm <path>` — materialize the bundled
//! noop harness WASM to a given path.
//!
//! Exists so `build.sh --push-pi-wasm` has something to push without
//! requiring `wat2wasm`/wabt on the host. Compiles the in-crate WAT
//! source to WASM and writes it. Removed once a real pi WASM is
//! consistently available on GHCR and this scaffolding isn't needed.

use std::path::PathBuf;

use xpressclaw_core::harness::HarnessImageResolver;

pub async fn run(path: PathBuf) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;

    // Ask the resolver to materialize its bundled fallback into a
    // temp cache dir, then copy that file to the requested path.
    // Avoids duplicating the WAT source in this crate.
    let tmp = tempfile::tempdir()?;
    let resolver = HarnessImageResolver::with_fallback(tmp.path().to_path_buf());
    // Any non-file, non-OCI ref triggers the fallback.
    let wasm_path = resolver.resolve("bundled-noop").await?;
    std::fs::copy(&wasm_path, &path)?;

    let size = std::fs::metadata(&path)?.len();
    println!(
        "Wrote bundled harness WASM to {} ({} bytes)",
        path.display(),
        size
    );
    Ok(())
}
