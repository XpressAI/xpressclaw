# Native runner images

XpressClaw publishes one product-specific image for each supported ACP agent:

| Image suffix | ACP product |
| --- | --- |
| `claude` | Claude Agent |
| `codex` | Codex |
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
package or platform archive pinned by the official ACP Registry. Run
`scripts/build-runner-images.sh` from the repository root to build every local
tag.

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

Repositories that use another SSH remote can opt into **Use my host SSH
agent**. XpressClaw forwards the live agent socket and read-only SSH
configuration/known-host files; it does not mount private keys. The retained
container is replaced when the host agent socket changes, so a desktop agent
restart does not leave the runner attached to a destroyed socket. This access
is intentionally disabled by default because every process in the runner can
request signatures from every key loaded in the forwarded agent.

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
