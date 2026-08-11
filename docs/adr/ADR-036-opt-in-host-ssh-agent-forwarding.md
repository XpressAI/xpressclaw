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
- uses Docker Desktop's `/run/host-services/ssh-auth.sock` bridge when the
  selected daemon identifies itself as Docker Desktop on macOS, while other
  runtimes mount the detected socket directly;
- bind-mounts only that Unix socket and the read-only host
  `~/.ssh/known_hosts` file when present, and materializes a private read-only
  configuration from `~/.ssh/config` plus recursively selected regular
  `Include` files under the host home directory; included files must contain
  only recognized OpenSSH client directives, so unknown, binary, and private
  key formats matched by broad globs are skipped and private keys are never
  exposed to the container;
- points Git's SSH transport at the forwarded socket, keeps new host keys in
  private Agent-scoped storage under XpressClaw's data directory so they
  survive container recreation, and uses the host known-host set as an
  additional trust source;
- uses a shared SELinux label for those explicit mounts and adds the socket's
  host group to the container, matching rootless Podman and Docker hosts; and
- includes opaque socket and known-host device/inode generations plus the
  effective materialized SSH configuration in the retained-container
  specification, so replacing any source or changing an included config
  recreates the Agent container on the next turn.

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
need the desktop `SSH_AUTH_SOCK` imported into its service environment. Host
keys learned through `accept-new` remain in that Agent's private runtime data
until the XpressClaw data directory is removed.
