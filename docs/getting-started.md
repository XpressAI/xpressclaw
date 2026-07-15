# Getting Started

## 1. Start the control plane

From the project you want xpressclaw to manage:

```bash
xpressclaw up
```

Open `http://localhost:8935`. An explicit `xpressclaw init` is optional; `up`
opens first-session setup when no configuration exists.

Use `xpressclaw up --detach` to keep the server in the background.

## 2. Create a native session

The session creator asks for:

- Codex, Claude Code, or OpenCode;
- the host project folder mounted at `/workspace`;
- whether to reuse the native CLI's host subscription login;
- optional image and extra folder overrides.

Each built-in product selects a separate minimal image. Xpressclaw never uses
an all-in-one `xpressclaw-native-runner` image.

## 3. Check readiness

Open the session. Its readiness panel checks the container runtime, product
image, project folder, command adapter, and subscription credentials. Use
**Prepare runner** to pull a missing published image. A locally built
`xpressclaw-runner-<product>:latest` image is accepted as a development
fallback.

The project folder name becomes the session's context label. There is no agent
name, persona, role, or Xpressclaw-authored system prompt.

## 4. Send work

Describe an outcome in the session. Xpressclaw queues it, launches a short-lived
native worker, and writes progress and results back to the durable timeline.
The session remains available while an attempt runs.

## 5. Automate work

- **Tasks** queue explicit work for a selected session.
- **Schedules** send work to a session on a cron schedule.
- **Workflows** coordinate multiple sessions, including implementation/review
  loops.

## Lifecycle commands

```bash
xpressclaw status
xpressclaw down
```

All product operations and configuration live in the web UI. The CLI only
manages the local control-plane process.
