# Repository guidance

## Rust formatting

- Use `./scripts/rustfmt.sh` to format Rust code and `./scripts/rustfmt.sh --check` to verify it.
- Do not run `cargo fmt --all`. Cargo follows the local `ready-agent-cog` path dependency despite the workspace exclusion and will modify the `external/ready-agent-cog` submodule.
- Treat everything under `external/` as third-party code unless a task explicitly targets it.

## Windows paths

- `Path::canonicalize()` returns the verbatim `\\?\C:\...` form on Windows. Strip it with `xpressclaw_core::paths::strip_verbatim` on every value that leaves the process: HTTP responses, stored config, container bind-mount sources, and user-facing error strings.
- Keep the verbatim path for filesystem calls. `\\?\` is what lifts the 260-character `MAX_PATH` limit and Rust's std does not re-add it, so stripping before `read_dir()` or `File::open()` trades a display bug for a failure on deeply nested folders. Strip at the boundary, not at the source.
