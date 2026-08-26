# Remote Access

XpressClaw separates the durable **control-plane instance** from its clients.
The instance owns Projects, queues, schedules, workflows, repositories, and
Agent processes. A Desktop window or browser only controls that state, so a
disconnect does not cancel work.

The safe default remains `127.0.0.1:8935`. Remote access is opt-in and has two
independent decisions:

1. who can reach the listener (loopback, trusted LAN/tailnet, SSH, firewall,
   or reverse proxy), and
2. whether XpressClaw also requires a password or per-start login token.

XpressClaw authentication protects the application; it does **not** provide
TLS or encrypt HTTP traffic.

## Direct Tailscale or trusted-LAN access

Direct unauthenticated access is intentionally supported when the operator
trusts every device and person that can reach the address. In **Settings →
Instance**, choose `0.0.0.0` (all IPv4 interfaces), `::` (all IPv6 interfaces),
or a specific interface address, leave authentication off, and confirm the
warning. Restart XpressClaw. The saved acknowledgement applies on subsequent
starts, so a normal Tailscale setup does not require a recurring CLI flag.

The equivalent one-start CLI override is:

```bash
xpressclaw up --bind 0.0.0.0 --allow-insecure-remote
```

This is a deliberate trust decision, not a security feature. Anyone who can
reach the port can read Project data, direct Agents, and use capabilities
assigned to those Agents. Do not publish this endpoint to the public internet
or an untrusted shared LAN.

## XpressClaw password or startup-token authentication

Enable **Require XpressClaw login** in **Settings → Instance**. You may set a
password there; XpressClaw stores only an Argon2id verifier in the instance's
restricted secret file. The password, verifier, and browser sessions are not
stored in `xpressclaw.yaml`, SQLite, Project sync, logs, or browser storage.

If authentication is enabled without a password, every server start creates a
new strong token. Foreground startup prints it once. `xpressclaw up --detach`
prints it in the invoking terminal while passing it to the child through an
anonymous pipe; it is never written to `server.log`, an argument, or an
environment variable. Restarting rotates the token and invalidates sessions.
At the login screen, enter the value printed after
`XPRESSCLAW_STARTUP_TOKEN=`.

Changing/removing the password or toggling authentication revokes existing
sessions. Removing a password requires a restart before login can resume with
a newly printed token. Bind, port, and authentication-mode edits likewise show
as restart-pending in Settings; effective running values remain visible.

Browser sessions use HttpOnly SameSite cookies and CSRF protection. Login
attempts are throttled by the directly connected peer address. XpressClaw does
not trust arbitrary forwarded client-address or protocol headers.

## SSH tunnel

Keep the server on loopback and run this from the client machine:

```bash
ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host
```

Open `http://localhost:8935` on the client. SSH supplies encryption and host
access control. Application authentication is optional in this topology. Use a
different first port, such as `9893`, if the client port is occupied.

## HTTPS reverse proxy

A reverse proxy may terminate TLS in front of XpressClaw. It must preserve
same-origin UI/API access, cookies, SSE, WebSockets, response streaming, and
attachment sizes, including the browser's `Origin` and `Sec-Fetch-Site`
headers. It may preserve the public `Host` or rewrite it to the upstream
listener. Bind XpressClaw to loopback when the proxy runs on the same host.
XpressClaw authentication may be used alone or in addition to proxy
authentication, but it never substitutes for TLS on an untrusted network.

Because externally terminated HTTPS is common, XpressClaw does not infer
cookie policy from untrusted `Forwarded` or `X-Forwarded-*` headers. Login and
Desktop ticket exchange instead use the browser's actual HTTPS `Origin` to set
the session cookie's `Secure` attribute. Restrict proxy-to-XpressClaw access at
the host/network layer, redirect HTTP to HTTPS, and enable HSTS at the proxy.

## Desktop profiles

Desktop always starts the automatic local instance at `~/.xpressclaw`. In
**Settings → Instance**, remote profiles can be added, health-checked, pinned
to an instance identity, selected, edited, or removed. Passwords and startup
tokens are stored in the operating-system keychain; the profile JSON contains
only the name, URL, identity, and authentication mode. A remote profile with
authentication off requires the same trusted-network confirmation.

The current Desktop release intentionally selects one profile for the whole
application. Switching closes secondary workspace windows before navigating
the main window, so no window silently remains connected to stale instance
state. The local sidecar remains running while a remote profile is selected.
An expired browser session returns to the login screen. If a selected remote
profile is unreachable or its saved credential is rejected during Desktop
startup, Desktop falls back to the automatic local profile and marks the
remote profile as needing credentials. Desktop exchanges a keychain credential
for a short-lived, single-use ticket and never returns the stored credential to
web content.

## Reconnection and runner callbacks

- Browser views reload durable data, and SSE/WebSocket clients reconnect using
  their normal session cookie. A stale or revoked session returns to login.
- A client disconnect does not change Task, Conversation, schedule, or
  workflow status.
- A server restart stops owned runtime processes, recovers interrupted work,
  rotates startup-token authentication, and invalidates browser sessions.
- Runner containers use a separate ephemeral callback listener protected by a
  random process capability. It is not a client endpoint and is not governed
  by browser sessions.
