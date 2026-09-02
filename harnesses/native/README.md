# Native runner images

XpressClaw publishes one product-specific image for each supported ACP agent:

| Image suffix | ACP product |
| --- | --- |
| `claude` | Claude Agent |
| `codex` | Codex |
| `deepseek-harness` | DeepSeek Harness via the maintained openma-ai ACP adapter |
| `github-copilot` | GitHub Copilot CLI |
| `junie` | Junie |
| `kimi` | Kimi CLI |
| `opencode` | OpenCode |
| `pi` | pi ACP |
| `qwen` | Qwen Code |
| `cline` | Cline |
| `cursor` | Cursor |
| `glm` | GLM Agent |
| `grok` | Grok Build |
| `kilo` | Kilo Code |
| `mistral-vibe` | Mistral Vibe |

`codex`, `claude`, and `opencode` retain dedicated Dockerfiles. The `npm` and
`binary` Dockerfiles are parameterized templates; CI supplies the exact
package or platform archive pinned by its source project or the ACP Registry. Run
`scripts/build-runner-images.sh` from the repository root to build every local
tag.

## DeepSeek Harness

The `deepseek-harness` images install the stable
`@openma/deepseek-harness-acp@0.4.24` package and start its standalone
`dsh-acp` stdio command. The adapter is maintained by openma-ai; it is not an
official DeepSeek package. Standalone mode is deliberate here: the runner has
to be reproducible and cannot depend on every host having an ACP profile under
`$DSH_HOME/profiles`.

The adapter package carries an exact, lockfile-built DeepSeek Harness runtime.
At image build time XpressClaw validates the archive against its independently
pinned SHA-256, reinstalls that exact lock for the target architecture, and
sets `DSH_PATH`/`NODE_PATH` to the resulting tree. This works around the stable
package's private-runtime module-resolution gap and replaces the publisher's
x64 native optional packages with the locked amd64 or arm64 variants, without
independently resolving mutable DSH dependencies. The parameterized image uses
Node 24, which satisfies the adapter's Node 22.15 minimum.

The build runs `verify-acp-stdio.mjs` against the real adapter and a loopback
DeepSeek-compatible stream without a paid model call. It verifies ACP v1
initialization, `auth_required` and Agent Auth, image ingestion, two
task/Conversation-style sessions on one process, a stdio MCP server, streamed
reasoning and text, plans, tools, file diffs, a permission escalation, active
cancellation, session list/load, persisted reload, and process shutdown.

Users authenticate on the host with `dsh-acp login` or through `dsh web`.
When **Use my existing login** is enabled, XpressClaw mounts host `~/.dsh`
read-write at `/home/node/.dsh`; it never copies that sensitive tree into the
image. XpressClaw also sets the adapter session root to
`/home/node/.dsh/acp-sessions`, keeping task and Conversation sessions inside
that one explicit mount. See
[`docs/deepseek-harness.md`](../../docs/deepseek-harness.md).

The default `runner` stage stays minimal. The build script also creates a
separate `xpressclaw-runner-<agent>-docker:latest` `runner-host` variant for
projects that need the host Docker-compatible engine.

These variants copy Docker CLI, Compose, and Buildx from the official Docker
CLI image. Xpressclaw mounts its detected Docker or rootless Podman Unix socket
only when a project explicitly enables host-engine access.

The control plane launches the image's ACP server and attaches over
bidirectional stdio. Tasks are sent with `session/prompt`, while fresh and
continued work use ACP session lifecycle methods. Agent commands and versions
follow the official ACP Registry.

## Codex presentation artifacts

The Codex images include the separate `xpressclaw-presentations` skill and a
read-only PptxGenJS 4.0.1, LibreOffice, and Poppler runtime. The Dockerfile
authors and renders a smoke deck during the image build. Exact image labels
gate discovery through ACP `additionalDirectories`, so an older or custom
image without the runtime remains usable but cannot advertise presentation
delivery. OpenAI's primary-runtime Presentations and Spreadsheets skills are
disabled in Codex ACP sessions because their host-provided
`load_workspace_dependencies`/`@oai/artifact-tool` contract is not available
there. See [`docs/presentations.md`](../../docs/presentations.md).

## Publishing

`Build & Push Runner Images` publishes all multi-architecture tags on a
push to `main` or a manual workflow dispatch. It then verifies every tag from
an unauthenticated job. GitHub creates a container package as private on its
first publication, so an XpressAI organization owner must change each new
package to **Public** once in its package settings. The release workflow will
not publish a desktop beta until all runner tags pass the anonymous check.

For a `github.com` origin, Xpressclaw discovers the host's existing `gh`
login and supplies Git credentials through `credential.helper`. The worker
still has the complete Git CLI. GitHub PR, review, check, and Actions
operations are exposed as one constrained, `gh`-shaped MCP tool; arbitrary
`gh api` and direct `gh` shell access are intentionally unavailable. Codex
sessions that receive this MCP also receive developer-level runtime guidance
that it satisfies generic skills' `gh` prerequisites, so a missing shell
binary must not block pull-request work. The MCP server advertises the same
substitution to other ACP agents.

Repositories that use another SSH remote can opt into **Share my host SSH
access**. XpressClaw mounts host `~/.ssh` read-write and forwards a live SSH
agent when one is available. A missing agent does not block file-based SSH
access. This setting is disabled by default because every process in the
runner can read or change the mounted SSH files and use every key exposed by a
forwarded agent.

## Customization

With **Use host login and harness configuration** enabled, XpressClaw mounts
the product's normal host configuration directory into `/home/node` in the
worker. This includes the product's subscription login, native settings, and
any supported skills, plugins, hooks, or custom agents.
Project-local configuration is read from the workspace. The session settings
UI can add other host-to-container mounts and environment values when an
extension uses a nonstandard location.

The control plane can also attach selected MCP servers through ACP. A stdio
server executable must exist at the configured absolute path inside this
image, so either extend the image or mount the server at that path. HTTP and
SSE servers are remote and do not increase the runner image size. ACP
advertised commands and session controls appear in task chat and workflows
after the harness has completed one discovery turn.

Pi is the exception at the ACP boundary: `pi-acp` accepts ACP MCP definitions
but does not connect them to Pi. The published Pi runner therefore includes
[`pi-mcp-adapter`](https://github.com/nicobailon/pi-mcp-adapter). XpressClaw
writes the effective per-task server list to a private runtime file, mounts its
directory read-only with shared SELinux relabeling, and starts the inner Pi RPC
process with that adapter. Existing Pi MCP files are still merged normally.
Pi receives a compact `mcp` gateway for discovery and individual calls, an
`mcpScript` JavaScript tool for batching or composing calls, and direct tools
for XpressClaw's control-plane and constrained GitHub servers. Credentials in
the generated runtime configuration are stored with owner-only permissions.

Runner images are intentionally product-specific. You can extend one while
the control-plane-managed development-environment interface is being built:

```dockerfile
FROM ghcr.io/xpressai/xpressclaw-runner-codex:latest

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends openjdk-21-jdk-headless \
    && rm -rf /var/lib/apt/lists/*
USER node
```

Long term, project toolchains should live in a separate development container
or remote environment. Xpressclaw can currently expose the host engine as a
pragmatic trusted mode so an agent can run Compose or create those environments
itself. Socket access is equivalent to the engine user's host-level authority;
the worker container prevents accidental damage to its own root filesystem,
but does not isolate the host engine or host paths mounted through it.
