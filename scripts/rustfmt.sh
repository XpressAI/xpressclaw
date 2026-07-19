#!/usr/bin/env bash
set -euo pipefail

# `cargo fmt --all` follows local path dependencies even when Cargo.toml lists
# them under workspace.exclude. Keep formatting scoped to XpressClaw packages
# so the external/ready-agent-cog submodule remains untouched.
packages=(
  xpressclaw-core
  xpressclaw-server
  xpressclaw-cli
  xpressclaw-tauri
)

format_args=()
for package in "${packages[@]}"; do
  format_args+=(--package "$package")
done

if (( $# > 0 )); then
  cargo fmt "${format_args[@]}" -- "$@"
else
  cargo fmt "${format_args[@]}"
fi
