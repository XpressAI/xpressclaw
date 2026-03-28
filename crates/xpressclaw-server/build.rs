// Temporary debug build script — prints what CARGO_MANIFEST_DIR resolves to
// and whether frontend/build/ is visible during compilation.
// Remove once the frontend embedding issue is resolved.
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let folder = format!("{manifest_dir}/../../frontend/build");
    let folder_path = std::path::Path::new(&folder);

    println!("cargo:warning=CARGO_MANIFEST_DIR = {manifest_dir}");
    println!("cargo:warning=frontend/build path = {folder}");

    match folder_path.canonicalize() {
        Ok(canonical) => {
            println!("cargo:warning=canonical path = {}", canonical.display());
            match std::fs::read_dir(&canonical) {
                Ok(entries) => {
                    let files: Vec<_> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect();
                    println!("cargo:warning=frontend/build contains {} entries: {:?}", files.len(), &files[..files.len().min(10)]);
                }
                Err(e) => println!("cargo:warning=read_dir failed: {e}"),
            }
        }
        Err(e) => {
            println!("cargo:warning=canonicalize failed: {e}");
            // Check if the non-canonical path exists
            println!("cargo:warning=exists (non-canonical): {}", folder_path.exists());
            if folder_path.exists() {
                if let Ok(entries) = std::fs::read_dir(folder_path) {
                    let count = entries.count();
                    println!("cargo:warning=non-canonical read_dir: {count} entries");
                }
            }
        }
    }
}
