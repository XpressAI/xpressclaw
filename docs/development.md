# XpressClaw Developer Guide

This guide covers source builds, repository architecture, native runner image
development, tests, and release mechanics. Installation and ordinary product
usage stay in the [README](../README.md).

## Prerequisites

- [Rust](https://rustup.rs/) stable toolchain
- [Node.js](https://nodejs.org/) 20+
- The [Tauri system dependencies](https://v2.tauri.app/start/prerequisites/)
  when building Desktop
- Docker or Podman for isolated ACP harnesses

Clone the repository and its submodules:

```bash
git clone --recurse-submodules https://github.com/XpressAI/xpressclaw.git
cd xpressclaw
```

## Build from source

Build the CLI/server and Desktop app:

```bash
./build.sh
```

The CLI/server is one executable with the web frontend embedded. Build it
without Desktop:

```bash
npm ci --prefix frontend
npm run build --prefix frontend
cargo build --release -p xpressclaw-cli
./target/release/xpressclaw up --instance "$HOME/.xpressclaw"
```

On Windows, use `build.ps1`; the CLI output is
`target\release\xpressclaw.exe`.

Individual Rust targets can also be built directly:

```bash
cargo build -p xpressclaw-core
cargo build -p xpressclaw-server
cargo build --release -p xpressclaw-cli
```

For a signed and notarized macOS Desktop build, use `./build-signed.sh`.

## Development mode

Run the Rust control plane and frontend development server separately:

```bash
# Terminal 1
cargo run -- up --instance .

# Terminal 2
cd frontend && npm run dev
```

The frontend proxies API calls to `localhost:8935`. `--instance .`
deliberately selects this checkout's development `xpressclaw.yaml`; omit it
when exercising normal `~/.xpressclaw` discovery.

## Repository architecture

XpressClaw is a Cargo workspace with four crates:

| Crate | Purpose |
|---|---|
| `xpressclaw-core` | Projects, Conversations, Agents, sessions, tasks, workflows, SQLite, and worker isolation |
| `xpressclaw-server` | Axum API, SSE streaming, and the embedded SvelteKit frontend |
| `xpressclaw-cli` | Local control-plane lifecycle, server, health, and synchronization commands |
| `xpressclaw-tauri` | Native Desktop app and system tray |

The main runtime pieces are:

```text
xpressclaw
+-- Axum server and embedded SvelteKit frontend
+-- Projects, Conversations, Agents, Tasks, shared memory, and files
+-- Durable SQLite state and event logs
+-- ACP clients and task/Conversation dispatchers
+-- Docker/Podman manager for retained per-Agent containers
```

Harnesses own their instructions, tools, reasoning loops, and subagents.
XpressClaw owns durable coordination, isolation, orchestration, and
presentation.

## Native runner images

Build all standard and host-engine runner variants:

```bash
./build.sh --with-runners
```

Build only selected products:

```bash
./build.sh --runner=codex --runner=claude
```

Or build one image directly:

```bash
docker buildx build --load \
  -f harnesses/native/codex/Dockerfile \
  -t xpressclaw-runner-codex:latest \
  -t localhost/xpressclaw-runner-codex:latest \
  harnesses/native
```

Set `CONTAINER_RUNTIME=docker` or `CONTAINER_RUNTIME=podman` when invoking
`scripts/build-runner-images.sh` to override automatic runtime detection.
Use the `runner-host` Dockerfile target only for trusted harnesses that need
Docker CLI, Compose, Buildx, and access to the host container engine.

Each newly published GHCR package must be public before a release can proceed.
The release workflow verifies that every runner image can be pulled without
credentials. See `harnesses/native/README.md` for image customization and the
separation between Agent runners and development environments.

## Harness capabilities

When host login is enabled, XpressClaw mounts the selected native harness's
configuration directory so its skills, plugins, hooks, custom agents, and user
settings remain available. Repository configuration is discovered through the
Agent workspace. Extra configuration trees can be attached with Agent volume
mounts and environment values.

MCP servers are defined once and enabled per harness. Stdio commands must be
absolute paths inside that harness image; remote HTTP and SSE endpoints do not
need to be installed in the image. After the first turn, composers and
workflow editors show the commands, modes, models, and reasoning controls the
ACP harness actually advertises.

## Tests and formatting

```bash
# Rust tests
cargo test -p xpressclaw-core -p xpressclaw-server -p xpressclaw-cli

# Frontend
npm --prefix frontend run check
cd frontend
npx playwright install chromium
npm run test:e2e
cd ..

# Installer
sh -n install.sh
bash scripts/test-install.sh

# Rust formatting and linting
./scripts/rustfmt.sh --check
cargo clippy \
  -p xpressclaw-core \
  -p xpressclaw-server \
  -p xpressclaw-cli \
  -p xpressclaw-tauri \
  --all-targets -- -D warnings
```

Use `./scripts/rustfmt.sh`, not `cargo fmt --all`; Cargo's workspace traversal
can otherwise modify the third-party `external/ready-agent-cog` submodule.

## Release versioning and artifacts

The release line comes from `[workspace.package]` in `Cargo.toml`. Verify that
Tauri, frontend, lockfile, and bundled MCP metadata agree with it:

```bash
node scripts/release-metadata.mjs --check
```

Automated prereleases use the next numeric build as the patch version. The
release workflow builds Desktop packages and standalone CLI/server archives
from the same binaries, then publishes `SHA256SUMS` for every asset.

The installer uses stable archive names:

- `xpressclaw-cli-aarch64-apple-darwin.tar.gz`
- `xpressclaw-cli-x86_64-apple-darwin.tar.gz`
- `xpressclaw-cli-x86_64-unknown-linux-gnu.tar.gz`
- `xpressclaw-cli-x86_64-pc-windows-msvc.zip`

By default, `install.sh` follows GitHub's `releases/latest`, which excludes
prereleases. Test or install a specific prerelease explicitly:

```bash
curl -fsSL https://raw.githubusercontent.com/XpressAI/xpressclaw/main/install.sh \
  | XPRESSCLAW_VERSION=v0.3.0-rc.1 sh
```
