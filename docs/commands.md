# CLI Command Reference

The xpressclaw CLI manages the local control-plane process. Product operations
such as messages, tasks, schedules, workflows, and runner configuration belong
in the web UI and REST API. Git-backed Project synchronization is the explicit
exception: it uses the `sync` commands below and never runs during ordinary
Project updates.

## `xpressclaw up`

Start the default local instance, web UI, scheduler, workflow engine, and ACP
task dispatcher:

```bash
xpressclaw up
xpressclaw up --detach
xpressclaw up --port 9000
```

The default UI is `http://localhost:8935`. XpressClaw discovers
`~/.xpressclaw`, creates the directory when needed, and opens first-run setup
when it has no configuration. If that default has not been configured, an
existing current-directory `xpressclaw.yaml` is still honored for backward
compatibility.

The server binds to `127.0.0.1` by default. Use an SSH tunnel or authenticated
HTTPS proxy for remote access. Because XpressClaw does not yet provide native
remote authentication, a non-loopback `--bind` is rejected unless the caller
also passes `--allow-insecure-remote` to acknowledge that an external security
layer is responsible for access control.

## `xpressclaw init` (optional/advanced)

Provision an instance directory before first launch. This does not add a
repository, Project, Agent, or runner image:

```bash
xpressclaw init
xpressclaw init /path/to/alternate-instance
```

Without a path, `init` targets `~/.xpressclaw`. Desktop users do not run it;
Desktop creates and starts that instance itself. Older scripts that relied on
the former current-directory default should pass `xpressclaw init .`
explicitly.

## Alternate instances

Use alternate instances only for an independent state, credential/security,
environment, or remote-host boundary:

```bash
xpressclaw up --instance /path/to/alternate-instance --port 9001
xpressclaw status --port 9001
xpressclaw down --instance /path/to/alternate-instance --port 9001
```

`--instance` always means the directory containing `xpressclaw.yaml`; it never
means an Agent's source workspace. `--workdir` remains a deprecated alias for
existing `up` and `down` scripts.

## `xpressclaw status`

Check the server and list durable Agents with their current queue state.

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

Pass the matching `--instance` when stopping an alternate detached instance.

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
