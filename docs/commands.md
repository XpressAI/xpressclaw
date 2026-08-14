# CLI Command Reference

The xpressclaw CLI manages the local control-plane process. Product operations
such as messages, tasks, schedules, workflows, and runner configuration belong
in the web UI and REST API. Git-backed Project synchronization is the explicit
exception: it uses the `sync` commands below and never runs during ordinary
Project updates.

## `xpressclaw init`

Create a control-plane directory, an empty `xpressclaw.yaml`, and the local
data directory. The argument names the control plane, not a repository being
managed. One control plane can manage many Project workspaces. This command
does not create an Agent or pull a runner image; those choices are made in the
UI.

```bash
xpressclaw init ~/.xpressclaw
xpressclaw init /path/to/control-plane
```

`init` is optional. Running `xpressclaw up` without a configuration opens the
first-session setup flow automatically. Desktop users do not run `init`;
Desktop owns `~/.xpressclaw/xpressclaw.yaml` and creates it during setup.

## `xpressclaw up`

Start the control plane, web UI, scheduler, workflow engine, and ACP task
dispatcher.

```bash
xpressclaw up
xpressclaw up --detach
xpressclaw up --port 9000
xpressclaw up --workdir /path/to/control-plane
```

The default UI is `http://localhost:8935`.

`--workdir` is the control-plane directory containing `xpressclaw.yaml`, not
an Agent's source workspace. Desktop always supplies `~/.xpressclaw` here.

## `xpressclaw status`

Check the server and list durable sessions with their current queue state.

```bash
xpressclaw status
xpressclaw status --port 9000
```

## `xpressclaw down`

Stop a detached control plane and its active worker processes. Retained project
containers remain stopped on disk for the next launch.

```bash
xpressclaw down
xpressclaw down --port 9000
```

## `xpressclaw sync`

Create a portable `.xpressclaw.yml` pointer, fetch shared Project state, or
publish local portable state through a separate Git repository:

```bash
xpressclaw sync init --project platform --remote git@github.com:org/data.git
xpressclaw sync fetch
xpressclaw sync publish
```

`--project` accepts a visible Project name or exact canonical ID; the legacy
`--project-id <ID>` spelling remains supported. From a project repository,
XpressClaw discovers a single control-plane checkout in a parent or sibling
directory when practical, then tries Desktop's `~/.xpressclaw/xpressclaw.yaml`.
Use `--project-dir` for the repository containing `.xpressclaw.yml` and
`--control-plane-dir` for the directory containing `xpressclaw.yaml` when
discovery is not unique. (`--workdir` remains an alias for
`--control-plane-dir`.) Fetch and publish require Git and use local
SSH-agent or credential-helper credentials. See [Git-backed Project
synchronization](project-sync.md) for the schema, conflict behavior, portable
data boundary, and security model.

## Removed legacy commands

The pre-native-runner `chat`, `tasks`, `memory`, `budget`, `sop`, and `logs`
commands were removed. Their old behavior depended on the retired in-house
agent layer. Use the structured session, work, workflow, and settings screens
instead.
