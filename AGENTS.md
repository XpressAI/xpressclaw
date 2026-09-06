# Repository guidance

## Rust formatting

- Use `./scripts/rustfmt.sh` to format Rust code and `./scripts/rustfmt.sh --check` to verify it.
- Do not run `cargo fmt --all`. Cargo follows the local `ready-agent-cog` path dependency despite the workspace exclusion and will modify the `external/ready-agent-cog` submodule.
- Treat everything under `external/` as third-party code unless a task explicitly targets it.

## Build storage

- Create task worktrees under `.worktrees/`, never under `target/`. Source checkouts must not live in a directory that `cargo clean` can delete.
- For direct Cargo checks and tests, reuse a target directory per repository and build environment instead of creating a new cache for each task. Keep host and container/toolchain caches separate. See `docs/development.md` for the shared-target command.
- Keep the compact dev/test defaults in `.cargo/config.toml`. Enable full debug information or incremental compilation only when needed.
- Before removing build artifacts, check for active builds and worktrees inside the target directory. Preserve source changes and running executables; remove only regenerable caches.
