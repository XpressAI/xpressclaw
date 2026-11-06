# Agent network proxy

**Settings → Proxy** configures one managed HTTP(S) gateway for an XpressCLAW
instance. Agents inherit the instance setting. Choose **Connect directly** in
an Agent's runner settings to exclude that Agent from domain and header rules.

New instance configurations enable the proxy. Existing configuration files
without a `proxy` section retain direct networking until the proxy is enabled
in Settings. Saving with the proxy enabled starts the service. It is also
started automatically when a proxied Agent begins work.

## Domain access

The default policy allows destinations except those explicitly blocked.
**Allow listed domains only** requires an entry in the allowed list; an empty
allowed list denies every external destination. Blocked entries take precedence
in both modes.

- `example.com` matches exactly that hostname, ignoring case.
- `*.example.com` matches subdomains, including deeper subdomains, but not
  `example.com` itself.
- Entries contain hostnames, without schemes, paths, or ports. International
  domains use their ASCII punycode representation.

**Skip HTTPS inspection** tunnels a host's TLS connection unchanged. Domain
checks still apply, including blocked entries. Header rules do not apply to
exempt hosts. This setting accommodates clients that pin server certificates.

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

The gateway uses a pinned multi-architecture mitmproxy image. Each installation
owns two networks: an internal network for proxied runners and an external
network for the gateway. Runners cannot make direct internet connections,
including direct UDP/QUIC or IPv6 connections. A stopped gateway leaves their
external connections blocked. Direct Agents use their ordinary runner network.

The signing key stays in the gateway's named volume. Only its public certificate
is mounted read-only into runners. Startup combines that certificate with the
runner's system CA bundle and configures Node, Python, curl, Git, and clients
using `SSL_CERT_FILE`. Upstream TLS certificate verification remains enabled.
Custom clients must support the configured proxy and CA trust, or use an explicit
host inspection exemption or Agent direct mode.

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

Run the gateway/network test with a directory shared with the container daemon:

```bash
XPRESSCLAW_PROXY_TEST_ROOT="$PWD/target/proxy-tests" \
  cargo test -p xpressclaw-core --lib \
  proxy::service::tests::isolated_gateway_roundtrip -- --ignored --exact --nocapture
```

The network test checks proxied access, blocked direct egress, policy updates,
callback relaying, behavior while the gateway is stopped, and CA persistence.
It removes its own containers, networks, and certificate volume afterward.
