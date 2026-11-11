# Agent network proxy

**Settings → Proxy** configures one managed HTTP(S) gateway for an XpressCLAW
instance. Agents inherit the instance setting. Choose **Connect directly** in
an Agent's runner settings to exclude that Agent from proxy and network rules.

New instance configurations enable the proxy. Existing configuration files
without a `proxy` section retain direct networking until the proxy is enabled
in Settings. Saving with the proxy enabled starts the service. It is also
started automatically when a proxied Agent begins work.

## Web domain access

The default policy allows destinations except those explicitly blocked.
**Allow listed domains only** requires an entry in the allowed list; an empty
allowed list denies every destination through the web proxy. Blocked entries take precedence
in both modes.

- `example.com` matches exactly that hostname, ignoring case.
- `*.example.com` matches subdomains, including deeper subdomains, but not
  `example.com` itself.
- Entries contain hostnames, without schemes, paths, or ports. International
  domains use their ASCII punycode representation.

**Skip HTTPS inspection** tunnels a host's TLS connection unchanged. Domain
checks still apply, including blocked entries. Header rules do not apply to
exempt hosts. This setting accommodates clients that pin server certificates.

**Web proxy ports** lists the destination ports the HTTP proxy may contact.
The defaults are 80 and 443. Add ports such as 8443 for other web services.
This check also applies to inspection exemptions and CONNECT requests. An
inspection exemption alone cannot open a tunnel on an unlisted port.

## Direct network access

The default SSH rule allows TCP port 22 to any destination. Ordinary SSH,
Git over SSH, SCP, and SFTP keep using their original hostnames and commands.
Disable the SSH rule to remove that permission, or change its destination/ports
to limit it to particular servers. Enabled rules are combined: a matching rule
allows the connection.

Add rules for other services using these fields:

| Field | Values |
|---|---|
| Protocol | TCP or UDP |
| Destination | One exact hostname, an IP address, a CIDR range, or `*` for any destination |
| Ports | Comma-separated ports and inclusive ranges, such as `5432,5000-5010` |
| Enabled | Turn a rule on or off while retaining its configuration |

The page includes PostgreSQL, Redis, and DNS presets. Network exceptions bypass
HTTP header filtering and are independent of the web domain lists. All other
direct outbound traffic is denied. Loopback, the gateway, authenticated
control-plane callbacks, and the engine's DNS resolvers remain available.

Rules govern new outbound connections; established connections finish normally.
Changes are loaded within a few seconds. Exact hostname rules resolve through
the engine's DNS resolvers, refresh every 30 seconds, and allow the resulting IP
addresses. A hostname that does not resolve contributes no permission.
Engine aliases such as `host.docker.internal` use the mappings captured before
the Agent starts; later Agent edits to hosts or resolver files do not expand access.

## Request headers

Rules run in order. Each rule specifies a header, a value matcher, an action,
and optionally a list of domains. Matchers are any value, exact value, contains
text, a regular expression, or contains an email address. Regular expressions
use the common Rust/Python syntax; lookaround and backreferences are excluded.
Each regex evaluation has a time budget; exceeding it blocks the request.

Actions remove the header, replace its entire value, or block the request.
Header names are case-insensitive. When any repeated value matches, removal or
replacement applies to every occurrence of that header. Routing, HTTP framing,
and proxy authentication headers cannot be modified.

The initial rules replace an email-bearing `User-Agent` with `XpressCLAW/1.0`
and remove `From`. Request URL paths, query strings, and bodies are not matched
against rules. Accepted request and response bodies stream through unchanged.
Blocked uploads are disconnected before their bodies are buffered or forwarded;
other blocked requests receive HTTP 403.

Recent events show the destination, action, and rule number. They exclude URL
paths, header values, and bodies. Rule changes affect subsequent requests.
Inspection exemptions affect new connections. Agent network changes take effect
when the Agent next starts a turn and its retained runtime is reconciled.

## Runtime

The gateway uses a pinned multi-architecture mitmproxy image. Each proxied Agent
shares a dedicated network namespace with an installation-owned firewall guard.
The guard installs IPv4 and IPv6 outbound rules before the Agent is started.
Only the guard receives NET_ADMIN; Agents drop NET_ADMIN and NET_RAW, including
when their container user is root. The guard has a read-only policy mount and
refreshes rules without restarting the Agent.

The pinned guard image supplies iptables and BusyBox. Its Docker daemon is never
started and no host engine socket is mounted. Guard ownership includes both the
installation and Agent IDs, and Agent deletion cleans up its unused guards.
Old internal networks remain available to retained environments during migration.
Direct Agents keep their ordinary runner network.

A stopped web gateway blocks proxied HTTP(S) requests while permitted direct
services remain usable. Unapproved direct web connections remain blocked.

The signing key stays in the gateway's named volume. Only its public certificate
is mounted read-only into runners. Startup combines that certificate with the
runner's system CA bundle and configures Node, Python, curl, Git, and clients
using `SSL_CERT_FILE`. Upstream TLS certificate verification remains enabled.
HTTP clients use the configured proxy and CA trust. Inspection exemptions
preserve the original TLS connection for pinned services.

A separate gateway port relays only to this instance's authenticated runner
callback listener. This preserves control-plane MCP calls on internal Podman
networks. The relay does not interpret or log callback content, and the listener
continues to verify each capability. Local collaboration tools use that same
callback path.

Host container-engine access requires the Agent's **Connect directly** setting;
otherwise an Agent could create containers with separate network access.
Remote MCP servers and hosted web tools make their subsequent requests in their
own environments. The instance gateway governs requests originating in its
proxied runner containers.

## Local verification

Run the real TLS policy tests without external websites:

```bash
docker run --rm --entrypoint python -v "$PWD:/work:ro" \
  docker.io/mitmproxy/mitmproxy:12.2.1@sha256:743b6cdc817211d64bc269f5defacca8d14e76e647fc474e5c7244dbcb645141 \
  /work/scripts/test-proxy.py
```

Build the shared test fixture and run the network tests with a directory shared
with the container daemon:

```bash
docker build -f crates/xpressclaw-core/tests/fixtures/environment.Dockerfile \
  -t xpressclaw-environment-test:local crates/xpressclaw-core/tests/fixtures
XPRESSCLAW_PROXY_TEST_ROOT="$PWD/target/proxy-tests" \
XPRESSCLAW_NETWORK_TEST_IMAGE=xpressclaw-environment-test:local \
  cargo test -p xpressclaw-core --lib proxy:: -- --ignored --nocapture
```

The tests cover real SSH authentication, TCP and UDP permissions, policy updates,
blocked unapproved ports, header filtering, callback relaying, CA persistence,
root capability restrictions, CONNECT port checks, and owned-resource cleanup.
They remove their own containers, networks, and certificate volumes afterward.
