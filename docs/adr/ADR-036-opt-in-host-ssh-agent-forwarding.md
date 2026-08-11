# ADR-036: Opt-in host SSH-agent forwarding

## Status

Accepted

## Context

Most existing workspaces are Git clones, and many use SSH remotes. Mounting
the workspace gives a retained Agent the repository and its Git metadata, but
not the host credential needed to fetch or push. Mounting `~/.ssh` would solve
that mechanically while also exposing every private-key file to the runner,
which is an unacceptable default trust expansion.

GitHub repositories already have a narrower path: the project-scoped GitHub
connector supplies an HTTPS credential helper and rewrites standard GitHub SSH
remote forms. Other Git hosts, custom SSH aliases, and GitHub projects without
connector access still need an explicit bridge.

## Decision

Each ACP Agent has an opt-in `runner.ssh_agent_forwarding` setting.

When enabled, XpressClaw:

- discovers `SSH_AUTH_SOCK` and common desktop/user-service Unix socket
  locations, and refuses to start the runner when none is live;
- bind-mounts only that Unix socket, plus read-only `~/.ssh/config` and
  `~/.ssh/known_hosts` files when present; private-key files are never read or
  mounted;
- points Git's SSH transport at the forwarded socket, keeps new host keys in
  the retained container, and uses the host known-host set as an additional
  trust source;
- uses a shared SELinux label for those explicit mounts and adds the socket's
  host group to the container, matching rootless Podman and Docker hosts; and
- includes an opaque socket device/inode generation in the retained-container
  specification, so replacing an agent socket at the same pathname rebinds a
  new project container on the next turn.

Setup inspects only Git metadata to flag workspaces with SSH remotes. Both
setup and Agent settings show whether a live host agent was detected and warn
that the capability applies to every loaded key.

## Consequences

Users can work with a previously cloned SSH repository without copying keys
into the container. Standard GitHub work continues to prefer the narrower
project-scoped HTTPS credential bridge.

Forwarding an SSH agent is not repository-scoped: a trusted process can ask it
to sign with any loaded key and can authenticate anywhere that key is
accepted. The option therefore remains off by default and must be enabled only
for trusted runner images and tasks. A service-launched XpressClaw process may
need the desktop `SSH_AUTH_SOCK` imported into its service environment.
