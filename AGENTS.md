# Repository guidance

## Rust formatting

- Use `./scripts/rustfmt.sh` to format Rust code and `./scripts/rustfmt.sh --check` to verify it.
- Do not run `cargo fmt --all`. Cargo follows the local `ready-agent-cog` path dependency despite the workspace exclusion and will modify the `external/ready-agent-cog` submodule.
- Treat everything under `external/` as third-party code unless a task explicitly targets it.
