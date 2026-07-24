//! Filesystem path helpers shared by the runtime and the HTTP API.

use std::path::{Path, PathBuf};

/// Resolve a path, keeping the original when it cannot be resolved.
pub fn canonical_or_original(path: &Path) -> PathBuf {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim(resolved)
}

/// Strip the `\\?\` prefix Windows `canonicalize()` adds to drive paths. Docker
/// Desktop's bind-mount parser rejects it, and clients render it verbatim.
/// `\\?\UNC\` shares keep their prefix; other platforms pass through as-is.
#[cfg(windows)]
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(not(windows))]
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn strips_drive_paths() {
        assert_eq!(
            strip_verbatim(PathBuf::from(r"\\?\C:\Users\user")),
            PathBuf::from(r"C:\Users\user")
        );
    }

    #[test]
    fn keeps_unc_and_plain_paths() {
        for path in [r"\\?\UNC\server\share", r"C:\Users\user"] {
            assert_eq!(strip_verbatim(PathBuf::from(path)), PathBuf::from(path));
        }
    }
}
