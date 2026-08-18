# Local collaboration services

XpressClaw can optionally run a local GitBucket forge and Jenkins build service
for Agents you explicitly authorize. It complements GitHub and does not alter
existing GitHub repositories, tools, review gates, or workflow waits.

## Install and start

Use the same Docker Desktop (macOS/Windows) or Docker Engine/compatible runtime
(Linux) required for Agent isolation. In Settings → Local collaboration:

1. Enable configuration and keep 127.0.0.1 unless a secured remote setup needs
   another address.
2. Choose unused ports. Defaults are 8088 for GitBucket and 8089 for Jenkins.
3. Save, then choose Install services.
4. Select the Agents allowed to use the services and save again.

Installation pulls pinned images, creates an installation-scoped bridge network
and two named volumes, replaces GitBucket's root/root password, creates a
non-admin Agent account, secures Jenkins, and provisions the build job.
Enabling configuration never starts containers by itself.

| Service | Browser default | Agent endpoint |
|---|---|---|
| GitBucket | http://127.0.0.1:8088 | http://gitbucket:8080 |
| Jenkins | http://127.0.0.1:8089 | http://jenkins:8080 |

Only assigned Agent containers join the network. Other Agents receive neither
network access nor collaboration tools.

## Agent workflow

An authorized Agent can create/read repositories, push a workspace branch,
open/read/comment on pull requests, trigger Jenkins, read build state/logs, and
cancel a running build. The initial Jenkins path builds public repositories.
Add a safe unattended script:

    # .xpressclaw/jenkins.sh
    set -eu
    cargo test --workspace

The job accepts only a managed GitBucket URL and ref and runs that script. It
does not accept an arbitrary command parameter.

GitBucket approval reviews, commit checks, Jenkins artifacts/rebuild, private
Jenkins clones, and automatic webhooks are not supported in this slice. Tools
report these limits instead of pretending full GitHub compatibility. Agents can
poll repository, issue, pull-request, and build state with the read tools and
explicitly trigger Jenkins after a push or pull request.

## Lifecycle and resources

Default pinned images are ghcr.io/gitbucket/gitbucket:4.46.1 and
jenkins/jenkins:2.568.1-jdk21. Settings shows exact container, network, and
volume names. GitBucket has a 1 GiB memory limit; the Jenkins controller has 2
GiB; and its isolated build Agent has 1 GiB. Each has two CPUs and an
unless-stopped policy. XpressClaw replaces the isolated Jenkins build Agent
before every accepted job and rejects overlapping triggers, so repository code,
background processes, and user-level Git configuration cannot cross build
boundaries. Docker health checks expose stopped,
starting, healthy, unhealthy, port-conflict, and image-pull states after
XpressClaw or Docker restarts.

- Start uses existing containers and volumes.
- Restart reconciles the saved ports, images, and container settings by
  recreating managed containers while preserving volumes and credentials.
- Stop preserves repositories, builds, and credentials.
- Upgrade pulls configured tags and recreates containers while preserving data.
- Reset requires exact confirmation and deletes both volumes and secrets.

Neither service is required or automatically enabled for existing users.

## Backup and restore

Stop services before taking a consistent named-volume backup. Back up the two
volumes shown in Settings together with:

    <instance data directory>/collaboration/collaboration-secrets.json

The secret file is mode 0600 on Unix. Never paste it into issues, prompts, logs,
or repositories. Restore volumes and secrets together. Restoring only one side
can make service accounts inaccessible.

## Security and remote access

Host ports bind to loopback by default. For remote instances, expose browser
URLs only through authenticated HTTPS or SSH. Agent endpoints are Docker aliases
and must not be published externally.

Jenkins has no privileged mode, Docker-in-Docker, or host Docker socket. The
controller has zero build executors. Repository scripts run on a dedicated
inbound build Agent that has no controller-data mount, so a build cannot modify
Jenkins configuration or credentials through `/var/jenkins_home`. Docker image
builds are outside this slice; use ordinary compiler/test jobs or separately
managed Jenkins build Agents. XpressClaw does not install GitBucket's
experimental CI plugin.

Authorized Agents use a non-admin GitBucket account through a streaming,
revocation-enforcing Git proxy. The shared forge bearer token, administrator
credential, and Jenkins credential stay server-side. Each assigned runner gets
an identity-bound collaboration capability; removing the assignment blocks the
next API or Git request even if its retained container is still on the network.
The capability is separate from the general callback token.

## Git mirroring is not failover

A Git mirror copies commits, branches, and tags. It does not copy pull requests,
reviews, approvals, issue comments, webhooks, or build/check records. This
release does not claim automatic GitHub outage failover.

## Troubleshooting

- Docker unavailable: start Docker Desktop or Docker Engine, then reload.
- Port conflict: choose different ports, then use Restart & apply configuration.
- Image unavailable: verify registry access and the explicit tag.
- Starting/unhealthy: inspect redacted service logs in Settings.
- Agent cannot resolve a service: assign access, save, and let its retained
  container recreate at the next safe launch.
- Jenkins reports a missing script: commit .xpressclaw/jenkins.sh to the ref.
- Private build fails: use a public local repository in this first slice.

## Opt-in integration verification

Maintainers can pull both images and exercise installation, persistent restart,
repository creation/push, and a complete Jenkins fixture build with:

    XPRESSCLAW_DOCKER_INTEGRATION=1 cargo test -p xpressclaw-core \
      docker_stack_survives_restart_and_builds_a_fixture -- --ignored --nocapture

The test is ignored by default because it downloads and runs both services.
