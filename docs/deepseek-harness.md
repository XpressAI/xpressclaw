# DeepSeek Harness

XpressClaw supports [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)
through the maintained
[`@openma/deepseek-harness-acp`](https://github.com/openma-ai/deepseek-harness-acp)
adapter. The adapter is maintained by openma-ai; it is not an official
DeepSeek adapter.

## Install and sign in

XpressClaw's runner image already contains the adapter and its pinned DSH
runtime. Nothing has to be installed on the control-plane host if `dsh web`
has already saved a model credential there. To use the adapter's host login
command, install both its supported DSH runtime and the adapter:

```bash
npm install --global @deepseek-ai/dsh@0.1.1-rc.2
npm install --global @openma/deepseek-harness-acp@0.4.24
dsh-acp login
```

Interactive login does not echo the key. Alternatively, install DeepSeek
Harness with the upstream one-shot command, then save the credential under
**Settings → Models**:

```bash
npx @deepseek-ai/dsh@0.1.1-rc.2 web
```

Both routes use `$DSH_HOME`, which defaults to `~/.dsh`, and store credentials
in `~/.dsh/.credentials.yaml`. DeepSeek currently labels DSH a developer
preview. XpressClaw therefore pins both the adapter and runtime instead of
following their moving tags during an image build.

DSH keeps its general settings at `~/.dsh/settings.yaml` and named profiles
under `~/.dsh/profiles/`. Standalone `dsh-acp` normally defaults its session
store to `~/.dsh-acp/sessions`; XpressClaw deliberately sets
`DSH_SESSION_ROOT=/home/node/.dsh/acp-sessions` in the built-in image so one
explicit `~/.dsh` mount preserves credentials, configuration, and ACP session
history together.

Select **DeepSeek Harness** while creating an Agent and leave **Use my existing
DeepSeek Harness login** enabled. XpressClaw mounts host `~/.dsh` read-write at
`/home/node/.dsh`. The directory is sensitive: it contains credentials as well
as settings and session logs. It is never copied into the runner image or
serialized into `xpressclaw.yaml`. Disable host login when an intentionally
isolated Agent must not receive those credentials.

## Runner model

The built-in kind is `deepseek-harness`; `dsh`, `dsh-acp`, and
`deepseek-harness-acp` are accepted aliases for older or hand-written
configuration. Its images are:

| Access | Image |
|---|---|
| Minimal | `ghcr.io/xpressai/xpressclaw-runner-deepseek-harness:latest` |
| Host container engine | `ghcr.io/xpressai/xpressclaw-runner-deepseek-harness-docker:latest` |

The image starts the adapter's standalone `dsh-acp` command. XpressClaw uses
standalone mode instead of installing an ACP profile into `~/.dsh`: the image
remains reproducible, a host login mount cannot hide its adapter installation,
and replacing a container does not require modifying the user's DSH profile.
The adapter's checksum-verified, bundled DSH runtime is materialized during
the image build. Its included lockfile is reinstalled for the target
architecture, keeping both amd64 and arm64 images pinned to the same dependency
graph while selecting the correct native packages.

## ACP behavior

- Sessions default to `workspace-write`. `read-only` and
  `danger-full-access` are also exposed as standard ACP modes. The latter
  disables DSH's inner sandbox and approval prompts; the runner container is
  still the project boundary unless host-engine access is explicitly enabled.
- Task and Project Conversation lanes use distinct ACP sessions on the same
  retained adapter process. DSH session logs under `~/.dsh` survive container
  replacement when host login is enabled, and standard `session/load` resumes
  them.
- PNG, JPEG, WebP, and GIF prompt images are supported. Plans, streamed text
  and reasoning, tool calls and diffs, command output, usage, permissions,
  models, effort, and Agent presets use standard ACP updates and config
  options.
- XpressClaw passes each enabled MCP server with `session/new`. The adapter
  supports stdio and streamable HTTP MCP servers; one failing MCP server does
  not end the ACP session. Legacy SSE MCP transport is not provided by this
  adapter.
- Interrupt and Stop use standard `session/cancel`; the adapter cancels the
  active DSH turn rather than merely hiding its output.
- Missing credentials surface as standard ACP `auth_required`. Sign in on the
  host and retry; do not add an API key to the Agent environment or
  XpressClaw configuration.

DeepSeek Harness does not currently advertise ACP multi-root sessions.
XpressClaw therefore supplies the Agent's one workspace as `cwd`; additional
volume mounts remain filesystem paths rather than additional ACP roots.

The adapter also defines optional `_dsh/cordis/*`, rich terminal-output, and
subagent-transcript extensions. XpressClaw does not negotiate those optional
extensions. The complete standard ACP path—sessions, prompts, MCP, images,
cancellation, authentication, tool output, and config options—remains
available without adapter-specific protocol branches.
