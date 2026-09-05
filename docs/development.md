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

## Build storage

`target/` contains compiler intermediates, dependency libraries, test binaries,
and caches from previous builds. It can be much larger than the compressed
application installer. Cargo does not automatically remove every artifact left
by changed features, dependencies, toolchains, or old source worktrees.

The checked-in `.cargo/config.toml` disables incremental compilation and uses
line-table debug information for development builds and tests. Backtraces retain
file names and line numbers. Recompiling a changed crate can take longer, and
inspecting variables in a debugger requires opting into full debug information:

```bash
CARGO_INCREMENTAL=1 CARGO_PROFILE_DEV_DEBUG=2 cargo build -p xpressclaw-cli
CARGO_INCREMENTAL=1 CARGO_PROFILE_TEST_DEBUG=2 cargo test -p xpressclaw-core
```

These are standard [Cargo profile overrides](https://doc.rust-lang.org/cargo/reference/profiles.html).
Release builds keep their existing profile.

Put task worktrees under `.worktrees/<task>` instead of inside `target/`. For
direct Cargo checks/tests across worktrees, reuse the main checkout's cache:

```bash
CARGO_TARGET_DIR="$(git rev-parse --path-format=absolute --git-common-dir)/../target" \
  cargo test -p xpressclaw-core
```

Use a separate shared target directory for container builds or a different
toolchain. Do not apply this override to `build.sh`/`build.ps1`: those packaging
scripts expect binaries in the current checkout's normal `target/` paths.

After stopping builds, `cargo clean --profile dev` clears local debug/test
artifacts while retaining release output. When using a shared target, pass the
same `CARGO_TARGET_DIR` for cleanup. Check `git worktree list` first: older
checkouts may contain source worktrees under `target/`, so deleting the entire
directory or using unrestricted `cargo clean` would delete that source too.

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

### API timestamp contract

All server timestamps represent UTC. SQLite-backed fields may be serialized as
`YYYY-MM-DD HH:mm:ss` without an explicit zone; RFC 3339 producers retain their
existing `Z` or numeric offset. Frontend code must parse both forms through
`frontend/src/lib/serverTime.ts` so a zone is added only to zone-less SQLite
values.

Work timing is phase-specific. `response_queued_at` anchors queue wait and
`response_started_at` anchors active response generation. Legacy `created_at`
and `started_at` fields remain available for compatibility but must not be used
as current-turn response-latency anchors when the explicit fields are present.

## Native runner images

Build all standard and host-engine runner variants:

```bash
./build.sh --with-runners
```

Build only selected products:

```bash
./build.sh --runner=codex --runner=deepseek-harness
```

Or build one image directly:

```bash
docker buildx build --load \
  -f harnesses/native/codex/Dockerfile \
  -t xpressclaw-runner-codex:latest \
  -t localhost/xpressclaw-runner-codex:latest \
  harnesses/native
```

The Codex Dockerfile verifies its presentation runtime by creating, rendering,
and validating a real PPTX during the build. After a custom build, confirm the
capability contract as well:

```bash
docker image inspect xpressclaw-runner-codex:latest \
  --format '{{ index .Config.Labels "io.xpressclaw.presentations" }} {{ index .Config.Labels "io.xpressclaw.presentations.pptxgenjs" }}'
docker run --rm xpressclaw-runner-codex:latest \
  /opt/xpressclaw/presentation-runtime/bin/xpressclaw-presentation-runtime
```

The expected values are `xpressclaw-pptx-v1`, `4.0.1`, and absolute paths
inside `/opt/xpressclaw/presentation-runtime`. See
[`docs/presentations.md`](presentations.md) for the publication boundary and
custom-image contract.

Parameterized npm runners, including DeepSeek Harness, should normally be
built through the shared script so the pinned package, runtime preparation,
and ACP smoke arguments remain identical to CI:

```bash
./scripts/build-runner-images.sh deepseek-harness
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

The DeepSeek Harness image smoke launches the real `dsh-acp` adapter against a
loopback DeepSeek-compatible stream. It verifies authentication, image
ingestion, two coexisting sessions, MCP propagation, streamed reasoning/text,
plans, tools, diffs, permissions, active cancellation, session list/load,
persistence across process restart, and clean shutdown without a paid or
external provider call.

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
