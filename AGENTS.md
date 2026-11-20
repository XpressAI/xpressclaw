# Repository guidance

## Rust formatting

- Use `./scripts/rustfmt.sh` to format Rust code and `./scripts/rustfmt.sh --check` to verify it.
- Do not run `cargo fmt --all`. Cargo follows the local `ready-agent-cog` path dependency despite the workspace exclusion and will modify the `external/ready-agent-cog` submodule.
- Treat everything under `external/` as third-party code unless a task explicitly targets it.

## Windows paths

- Keep canonical paths for filesystem access and containment checks, including Windows extended-length paths.
- Use `xpressclaw_core::external_tools::path_for_external_tool` for native-tool arguments and paths displayed in API responses or user-facing errors. It converts both verbatim drive and UNC paths; do not duplicate that conversion.

## Build storage

- Create task worktrees under `.worktrees/`, never under `target/`. Source checkouts must not live in a directory that `cargo clean` can delete.
- For direct Cargo checks and tests, reuse a target directory per repository and build environment instead of creating a new cache for each task. Keep host and container/toolchain caches separate. See `docs/development.md` for the shared-target command.
- Keep the compact dev/test defaults in `.cargo/config.toml`. Enable full debug information or incremental compilation only when needed.
- Before removing build artifacts, check for active builds and worktrees inside the target directory. Preserve source changes and running executables; remove only regenerable caches.
