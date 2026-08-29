# Getting Started

## 1. Install and start your local instance

### Desktop

Install the `.dmg`, `.exe`/`.msi`, `.deb`, or `.rpm` for your platform from the
official [Releases](https://github.com/XpressAI/xpressclaw/releases) page and
launch XpressClaw. It starts its bundled control plane from
`~/.xpressclaw`, opens setup, and stays available in the system tray. Do not
run `xpressclaw init` or `xpressclaw up` separately. The Desktop package does
not install the CLI on `PATH`.

### Headless CLI/server

Beginning with stable release `0.3.0`, install the standalone CLI/server on
Apple Silicon or Intel macOS and x64 Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/XpressAI/xpressclaw/main/install.sh | sh
```

The installer follows GitHub's latest stable release, ignores prereleases,
verifies the release checksum, and writes `xpressclaw` to `~/.local/bin`.
Add that directory to `PATH` if needed, then run:

```bash
xpressclaw up
```

Open `http://localhost:8935`. The CLI automatically uses
`~/.xpressclaw` and opens setup when it has no configuration. `init` is not a
normal onboarding step; it remains available to provision the default
instance ahead of time or create an advanced alternate instance.

Use `xpressclaw up --detach` to keep the server in the background.
Windows users should install Desktop; stable releases also include a
standalone x64 CLI/server `.zip` for manual installation. Developers building
from source should use the [Developer Guide](development.md).

## 2. Add a repository and Agent

Complete first-run setup. An existing repository is an Agent workspace, not an
XpressClaw instance directory. Leaving the folder empty creates a durable,
isolated managed workspace for that Agent. The Agent creator asks for:

- an Agent name;
- Codex, Claude Code, DeepSeek Harness, OpenCode, or another ACP-compatible harness;
- the host project folder mounted at `/workspace`;
- whether to reuse a built-in harness's host subscription login;
- optional image, ACP server command, and extra folder overrides.

Each built-in product selects a separate minimal image. Xpressclaw never uses
an all-in-one `xpressclaw-native-runner` image.

For DeepSeek Harness, install its supported DSH runtime plus the maintained
openma-ai adapter on the control-plane host and run `dsh-acp login`, or save the
same credential through `dsh web` (Settings → Models). Both write under
`~/.dsh`; the runner image itself already contains both packages. Enable **Use
my existing DeepSeek Harness login** when creating the Agent; XpressClaw mounts
that directory read-write so credential refreshes and native session logs
survive runner replacement. Do not put an API key in XpressClaw configuration.
See [DeepSeek Harness](deepseek-harness.md) for exact host commands,
permissions, MCP, images, and session behavior.

First-run setup creates a Project around the Agent. A Project is the durable
collaboration and memory boundary and can contain more Agents, Conversations,
Tasks, and workflows. One instance can manage many Projects and repositories.

## 3. Check readiness

Open the agent. Its readiness panel checks the container runtime, ACP harness
image, project folder, server command, and subscription credentials. Use
**Prepare runner** to pull a missing published image. A locally built
`xpressclaw-runner-<product>:latest` image is accepted as a development
fallback.

Runtime selection is automatic. An explicit `DOCKER_HOST` wins; otherwise
Xpressclaw prefers a live user-level Docker Desktop or rootless Podman socket
before falling back to the platform Docker endpoint. Readiness and server
settings show the selected runtime and socket so local images are always
looked up in the same image store used to launch workers.

The chosen agent name labels its durable context. Legacy or manually edited
configuration without a name falls back to the workspace folder. The label is
not a persona, role, or Xpressclaw-authored system prompt.

## 4. Start a Conversation or send work

Start a Project Conversation with one or more Agents for shared coordination.
Conversation ACP sessions remain available while task work is running. Use
**Continue with task** for a durable outcome, or describe an outcome directly
to an Agent from **New Work**. XpressClaw queues task work, lazily starts and
initializes an ACP server inside the Agent's retained container, sends
`session/prompt`, and writes standard progress, plans, tool activity, and
results back to the durable timeline. Later task turns reuse that process and
its live ACP sessions along with installed tools, caches, `/home/node`, and
`/tmp`.

Open an Agent's **Files** tab to browse and edit its configured workspace with
Monaco, inspect current Git changes and diffs, or open a terminal in its
retained container. The terminal becomes available after the Agent has run its
first task. Task details also show the workspace's current changed files and
link directly to them in the editor.

### Clone first, attach GitHub later

A blank Agent can clone a repository during a Task or Conversation. The
configured workspace remains its writable security boundary, while the cloned
checkout becomes its narrower **active repository**:

1. Ask the Agent to clone into its workspace. It can immediately call the
   bundled GitHub tool with that checkout's absolute container directory as
   `cwd`; XpressClaw validates the Git root and origin, obtains only that
   repository's credential, and persists the selection before running `gh`.
   Without that call, exactly one eligible checkout is adopted automatically
   at the next safe turn boundary.
2. If more than one checkout exists and `cwd` is omitted, the GitHub tool
   returns safe relative candidates rather than guessing. Choose one under
   **Agent → Environment → Active repository**, or let the Agent propose the
   checkout with its XpressClaw control tool. Those choices apply next turn.
3. After live `cwd` resolution or a queued choice, XpressClaw recreates the
   retained ACP session on the next turn with the repository as its working
   directory. The same Task, messages, and managed review state remain.
4. For a GitHub origin, compatible built-in runner, and available GitHub
   credential, the constrained GitHub MCP works immediately through `cwd` and
   defaults to the active repository on later calls and turns.

The status card distinguishes no repository, multiple candidates, non-GitHub
origins, missing credentials, incompatible/custom images, an explicit GitHub
MCP override, and an attached bundled MCP. Clearing a selection disables
automatic re-adoption until another repository is selected. If a checkout is
deleted or its origin changes, XpressClaw invalidates its runtime identity and
shows the resulting diagnostic instead of switching to an unrelated clone.

## 5. Automate work

- **Tasks** queue explicit work for a selected Agent.
- **Automations → Schedules** send work to an Agent once or on a cron schedule.
- **Automations → Workflows** coordinate Agents through implementation/review,
  goal, and custom loops; workflows started in a Conversation report there.

## 6. Manage or delete a Project

Use **Project settings** to rename a Project or permanently delete it. Deletion
shows current counts and requires the exact Project name because it cancels
active work and removes the Project's Agents, conversations, tasks, messages,
memory, workflow runs, schedules, and runtime containers. Source repositories,
host workspace folders, and shared workflow definitions are preserved. See
[Projects and deletion](projects.md) for the complete boundary, retry behavior,
and explicit API acknowledgement.

## 7. Reconnect from another device

The browser or Desktop window is only a client. Closing it does not stop the
control plane or queued work. XpressClaw listens on loopback by default. An SSH
tunnel works without changing the listener:

```bash
ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host
```

Open `http://localhost:8935` on the client device. For direct Tailscale or a
fully trusted LAN, **Settings → Instance** can save `0.0.0.0` or `::` after an
explicit no-auth warning. You can instead enable a password or per-start token
there. Desktop users can save the resulting remote URL as an instance profile;
its credential stays in the OS keychain. XpressClaw authentication does not
provide TLS. See [Remote access](remote-access.md) for all supported topologies,
reconnection, and profile behavior.

## Lifecycle commands

```bash
xpressclaw status
xpressclaw down
```

All product operations and configuration live in the web UI. The CLI only
manages the local control-plane process.
