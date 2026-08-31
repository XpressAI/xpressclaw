use std::path::{Path, PathBuf};

/// Return a path that can safely be passed to a native external tool.
///
/// Windows `canonicalize()` returns verbatim paths such as `\\?\C:\...` and
/// `\\?\UNC\server\share\...`. Those paths are useful for filesystem
/// containment checks, but tools including Git and Docker Desktop do not
/// consistently accept them as command-line arguments.
pub(crate) fn path_for_external_tool(path: &Path) -> PathBuf {
    // Recognized verbatim prefixes are unambiguous, so perform the conversion
    // on every host. This lets Linux and macOS exercise both the conversion
    // and its external-tool call sites without requiring a Windows runner.
    let Some(path) = path.to_str() else {
        return path.to_path_buf();
    };
    if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{unc}"));
    }
    match path.strip_prefix(r"\\?\") {
        Some(drive_path)
            if drive_path.as_bytes().get(1) == Some(&b':')
                && drive_path
                    .as_bytes()
                    .get(2)
                    .is_some_and(|separator| matches!(separator, b'\\' | b'/')) =>
        {
            PathBuf::from(drive_path)
        }
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_verbatim_paths_are_safe_for_external_tools_on_every_host() {
        assert_eq!(
            path_for_external_tool(Path::new(r"\\?\C:\workspace\project\.git")),
            PathBuf::from(r"C:\workspace\project\.git")
        );
        assert_eq!(
            path_for_external_tool(Path::new(r"\\?\d:/workspace/project/.git")),
            PathBuf::from(r"d:/workspace/project/.git")
        );
    }

    #[test]
    fn windows_unc_verbatim_paths_are_safe_for_external_tools_on_every_host() {
        assert_eq!(
            path_for_external_tool(Path::new(r"\\?\UNC\server\share\project\.git")),
            PathBuf::from(r"\\server\share\project\.git")
        );
    }

    #[test]
    fn external_tool_paths_preserve_non_verbatim_inputs() {
        for path in [
            Path::new(r"C:\workspace\project"),
            Path::new(r"\\server\share\project"),
            Path::new("/workspace/project"),
            Path::new(r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\project"),
        ] {
            assert_eq!(path_for_external_tool(path), path);
        }
    }
}
