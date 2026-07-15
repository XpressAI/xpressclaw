# CLI Command Reference

The xpressclaw CLI manages the local control-plane process. Product operations
such as messages, tasks, schedules, workflows, and runner configuration belong
in the web UI and REST API rather than a second command interface.

## `xpressclaw init`

Create an empty `xpressclaw.yaml` and the local data directory. This command
does not create a session or pull a runner image; those choices are made per
session in the UI.

```bash
xpressclaw init
xpressclaw init /path/to/project
```

`init` is optional. Running `xpressclaw up` without a configuration opens the
first-session setup flow automatically.

## `xpressclaw up`

Start the control plane, web UI, scheduler, workflow engine, and native worker
dispatcher.

```bash
xpressclaw up
xpressclaw up --detach
xpressclaw up --port 9000
xpressclaw up --workdir /path/to/project
```

The default UI is `http://localhost:8935`.

## `xpressclaw status`

Check the server and list durable sessions with their current queue state.

```bash
xpressclaw status
xpressclaw status --port 9000
```

## `xpressclaw down`

Stop a detached control plane and its active short-lived worker containers.

```bash
xpressclaw down
xpressclaw down --port 9000
```

## Removed legacy commands

The pre-native-runner `chat`, `tasks`, `memory`, `budget`, `sop`, and `logs`
commands were removed. Their old behavior depended on the retired in-house
agent layer. Use the structured session, work, workflow, and settings screens
instead.
