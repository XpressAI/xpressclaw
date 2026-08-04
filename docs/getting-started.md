# Getting Started

## 1. Start the control plane

From the project you want xpressclaw to manage:

```bash
xpressclaw up
```

Open `http://localhost:8935`. An explicit `xpressclaw init` is optional; `up`
opens first-session setup when no configuration exists.

Use `xpressclaw up --detach` to keep the server in the background.

## 2. Create an agent

The agent creator asks for:

- a durable agent name;
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

## 4. Send work

Describe an outcome for the agent. Xpressclaw queues it, lazily starts and
initializes an ACP server inside the agent's retained project container, sends
`session/prompt`, and writes standard progress, plans, tool activity, and
results back to the durable timeline. Later turns reuse that process and its
live ACP sessions along with installed tools, caches, `/home/node`, and `/tmp`.

## 5. Automate work

- **Tasks** queue explicit work for a selected agent.
- **Automations → Schedules** send work to an agent once or on a cron schedule.
- **Automations → Workflows** coordinate agents through implementation/review,
  goal, and custom loops.

## Lifecycle commands

```bash
xpressclaw status
xpressclaw down
```

All product operations and configuration live in the web UI. The CLI only
manages the local control-plane process.
