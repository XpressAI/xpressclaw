# Getting Started

## 1. Start the control plane

From the project you want xpressclaw to manage:

```bash
xpressclaw up
```

Open `http://localhost:8935`. An explicit `xpressclaw init` is optional; `up`
opens first-session setup when no configuration exists.

Use `xpressclaw up --detach` to keep the server in the background.

## 2. Create a Project and Agent

Create a Project for the work, then add one or more Agents. The Agent creator
asks for:

- an Agent name;
- Codex, Claude Code, OpenCode, or another ACP-compatible harness;
- the host project folder mounted at `/workspace`;
- whether to reuse a built-in harness's host subscription login;
- optional image, ACP server command, and extra folder overrides.

Each built-in product selects a separate minimal image. Xpressclaw never uses
an all-in-one `xpressclaw-native-runner` image.

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

## Lifecycle commands

```bash
xpressclaw status
xpressclaw down
```

All product operations and configuration live in the web UI. The CLI only
manages the local control-plane process.
