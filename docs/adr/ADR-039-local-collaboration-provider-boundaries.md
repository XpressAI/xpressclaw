# ADR-039: Local Forge and Build Provider Boundaries

## Status

Accepted

## Context

GitHub is coupled to repository URL parsing, credential discovery, the bundled
MCP, managed task review polling, durable workflow waits, and some UI copy.
Ordinary shell Git is provider-neutral, but the managed credential helper is
not. GitBucket offers only a subset of GitHub APIs, while Jenkins has a
different build lifecycle.

## Decision

Add two capability-reporting seams:

- ForgeProvider covers repositories, issues, pull requests, comments/reviews,
  events, and commit statuses.
- BuildProvider covers trigger, state, logs, artifacts, cancel, and retry.

This first implementation deliberately supports only verified capabilities.
GitBucket supports repository and pull-request create/read plus discussion
comments. Review approvals and commit statuses report unsupported. Jenkins
supports trigger, state, log tailing, and cancellation through one constrained
job. Artifacts and plugin-dependent rebuild report unsupported. Existing GitHub
discovery, tools, workflow waits, and managed-review completion remain unchanged
and GitHub-only.

Local collaboration is an instance-scoped opt-in. Enabling configuration alone
never starts containers. XpressClaw uses its existing Bollard connection to
manage pinned images, one installation-scoped bridge network, and separate
persistent volumes. Host interfaces bind to loopback by default. Only assigned
Agents join the network and receive a separate collaboration capability.

Administrator secrets stay in a mode-0600 local file. GitBucket's root/root
password is replaced during setup before a non-admin service account is made.
Jenkins is bootstrapped without the unlock-secret flow; its bootstrap password
environment is removed by recreating the container after persistence.

An authorized Agent can push through a temporary askpass helper into a
streaming control-plane Git proxy. The proxy rechecks the Agent's current
assignment on every request and adds the non-admin GitBucket bearer token only
to the server-side upstream request. Shared forge credentials never enter the
Agent container. Jenkins receives no privileged mode, Docker-in-Docker, or host
Docker socket. Its no-plugin job accepts only a managed GitBucket URL and ref
and runs .xpressclaw/jenkins.sh from a public repository.

## Consequences

The slice supports repositories, pull requests, comments, branch pushes, and
ordinary builds while provider differences stay visible. It is not GitHub
outage failover: Git mirrors carry commits, branches, and tags, not pull
requests, reviews, issue comments, or checks.

Stop preserves data. Upgrade recreates containers but retains volumes.
Destructive Reset is separate and requires an exact confirmation.

## Follow-up work

1. Map managed review gates and workflow waits onto provider capabilities.
2. Add authenticated GitBucket-to-Jenkins webhooks and commit statuses after
   compatibility testing.
3. Add artifact APIs and a pinned Pipeline/Git plugin manifest if its security
   and maintenance costs are accepted.
4. Add repository-scoped credentials for private Jenkins clones.
5. Design explicit mirroring/outage behavior for non-Git metadata.
6. Add remote service URL profiles and TLS proxy validation.
