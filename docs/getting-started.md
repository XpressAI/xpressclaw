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
XpressClaw instance directory. The Agent creator asks for:

- an Agent name;
- Codex, Claude Code, OpenCode, or another ACP-compatible harness;
- the host project folder mounted at `/workspace`;
- whether to reuse a built-in harness's host subscription login;
- optional image, ACP server command, and extra folder overrides.

Each built-in product selects a separate minimal image. Xpressclaw never uses
an all-in-one `xpressclaw-native-runner` image.

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

## 5. Automate work

- **Tasks** queue explicit work for a selected Agent.
- **Automations → Schedules** send work to an Agent once or on a cron schedule.
- **Automations → Workflows** coordinate Agents through implementation/review,
  goal, and custom loops; workflows started in a Conversation report there.

## 6. Reconnect from another device

The browser or Desktop window is only a client. Closing it does not stop the
control plane or queued work. XpressClaw listens on loopback by default and has
no built-in remote authentication yet, so connect through SSH rather than
opening its port directly:

```bash
ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host
```

Open `http://localhost:8935` on the client device. See
[Remote access](remote-access.md) for authenticated reverse-proxy guidance,
reconnection behavior, and current Desktop limitations.

## Lifecycle commands

```bash
xpressclaw status
xpressclaw down
```

All product operations and configuration live in the web UI. The CLI only
manages the local control-plane process.
