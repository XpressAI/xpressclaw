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
attempts are throttled by the directly connected peer address. A reverse proxy
running on the same host may provide the client address for throttling as
described below; XpressClaw ignores forwarded identity from non-loopback peers.

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

For per-client login throttling, have the same-host proxy replace
`X-Forwarded-For` with its observed client address or append that address as
the final value. XpressClaw accepts only the final valid address and only when
the directly connected peer is loopback. It ignores this header from network
peers and never uses it for authentication, cookie policy, or authorization.

Because externally terminated HTTPS is common, XpressClaw does not infer
cookie policy from `Forwarded` or `X-Forwarded-*` headers. Browser login uses
the browser's actual HTTPS `Origin` to set the session cookie's `Secure`
attribute; native Desktop uses the selected profile's HTTPS scheme when it
installs the same HttpOnly cookie. Restrict proxy-to-XpressClaw access at the
host/network layer, redirect HTTP to HTTPS, and enable HSTS at the proxy.

## Desktop profiles

Desktop always starts the automatic local instance at `~/.xpressclaw`. In
**Settings → Instance**, remote profiles can be added, health-checked, pinned
to a cryptographic instance identity, selected, edited, or removed. Desktop
verifies a fresh signed challenge before using a saved credential; a recorded
bootstrap response or instance ID is not sufficient. Passwords and startup
tokens are stored in the operating-system keychain and sent only through a
one-use encrypted channel whose ephemeral key is signed by the pinned instance
identity. A relaying endpoint therefore sees only ciphertext, not the saved
credential, during the native exchange. The profile JSON contains only the
name, URL, public identity, and authentication mode. A remote profile with
authentication off requires the same trusted-network confirmation.
Desktop rechecks this policy whenever an active remote page loads, so a remote
that restarts with authentication disabled returns to a blocked review screen
instead of inheriting access from an earlier authenticated session.

Automatic keychain login is available only for the exact local HTTP sidecar
whose listeners were started by the current Desktop process. Remote profiles
use the normal browser login, including over HTTPS: a replacement origin could
relay the separately signed XpressClaw identity proof and receive any cookie
installed for that origin, while native code cannot prove that the browser's
connection terminates at the same instance. Direct HTTP over an
operator-trusted LAN or tailnet remains supported through that login form.

The current Desktop release intentionally selects one profile for the whole
application. Switching closes secondary workspace windows before navigating
the main window and clears every XpressClaw browser-session cookie before the
new instance receives a browser request. Browser cookies are scoped by hostname
rather than port, so this serial session boundary prevents instances such as
`localhost:8935` and `localhost:9000` from receiving or overwriting each
other's sessions. A native navigation guard also rejects browser Back/Forward
history or any other top-level navigation to a deselected origin before it can
make a request with the current session. It also means changing profiles signs
the browser out of the previous instance. Desktop starts on a non-network
bootstrap page and applies the same cleanup before its first instance
navigation. The local sidecar remains running while a remote profile is
selected.
An expired browser session returns to the login screen. Desktop falls back to
the automatic local profile only when a selected remote is unreachable or
cannot prove its pinned identity. A proved remote opens its browser login
without first validating saved keychain material, so a rotated startup token
can be entered directly; a successful login refreshes the saved credential and
authentication mode. For the managed local origin eligible for automatic
login, the server creates browser-session material only inside the signed
one-use encrypted channel. Desktop installs the session as an HttpOnly cookie
and returns only success/failure to web content; no password, startup token,
session token, or redeemable bearer ticket crosses the native command
boundary.

This credential channel protects the saved secret during Desktop login; it
does not add TLS or protect the browser session from an active network relay.
Use HTTPS, an SSH tunnel, or a trusted tailnet path when the network itself is
not trusted.

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
