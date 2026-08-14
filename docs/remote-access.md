# Remote Access

XpressClaw separates the durable **control-plane instance** from its
**clients**. The instance owns configuration, Project data, queues, schedules,
workflows, and Agent processes. A Desktop window or browser only presents and
controls that state. Closing a client, suspending a laptop, or briefly losing
the network does not cancel work.

## Supported topology today

Run XpressClaw on the machine that has the repositories, container runtime,
harness logins, and credentials its Agents need. The server listens on
`127.0.0.1` by default. Connect other devices through an authenticated
transport rather than exposing port 8935 directly.

Runner containers call back through a separate ephemeral host-gateway
listener. Every request on that listener requires a random per-process
capability injected only into XpressClaw's bundled runner MCP processes; it is
not a second client endpoint and its port is not stable.

### SSH tunnel

From the client machine:

```bash
ssh -N -L 8935:127.0.0.1:8935 user@control-plane-host
```

Open `http://localhost:8935` on that client. The server remains loopback-only
on the control-plane host, and SSH supplies host authentication, encryption,
and access control. A phone can use an SSH client that supports local port
forwarding, although an authenticated HTTPS endpoint is usually more
convenient for regular mobile use.

If local port 8935 is occupied, choose another client-side port:

```bash
ssh -N -L 9893:127.0.0.1:8935 user@control-plane-host
```

Then open `http://localhost:9893`.

### Authenticated HTTPS proxy

An operator may place a reverse proxy on the control-plane host in front of
`127.0.0.1:8935`. The proxy must provide TLS and real user authentication,
preserve same-origin access to the UI and `/api`, support streaming responses
and WebSockets, and use request-size limits compatible with XpressClaw file
attachments.

XpressClaw itself does not yet authenticate browser users or authorize API
operations. Network reachability alone is therefore not an adequate security
boundary. Anyone who can reach the API can direct Agents that may edit
repositories, use mounted credentials, or control an explicitly enabled host
container engine.

## Non-loopback binds

The CLI refuses a non-loopback bind unless the operator explicitly
acknowledges the missing application authentication:

```bash
xpressclaw up \
  --bind 0.0.0.0 \
  --allow-insecure-remote
```

This flag does not make the endpoint secure. Use it only when an external,
tested access-control layer protects every route. Never publish the raw port
to a LAN or the internet.

## Reconnection and restart behavior

- Browser views reload durable state through the API. Polling and event
  streams resume when connectivity returns; terminals expose an explicit
  reconnect action.
- Client disconnects do not change Task, Conversation, schedule, or workflow
  status.
- A control-plane process restart is different from a client disconnect. It
  stops owned worker processes, recovers interrupted durable work into the
  queue, and resumes workflow bookkeeping when the server starts again.
- Connect to the same instance to see the same data. A different instance has
  a different configuration and database even if it runs on the same host.

## Desktop limitations

Desktop currently owns one bundled local instance at `~/.xpressclaw`. Its
single application process and all workspace windows connect to that local
instance. It cannot yet save a remote URL, authenticate to a remote instance,
switch connection profiles, or bind different windows to different instances.

Until native connection profiles land, use a normal browser over an SSH tunnel
or authenticated HTTPS proxy for remote instances. The intended future model
is a client-side profile containing an instance URL and protected credential;
that profile is distinct from the server-side instance directory.

See [ADR-038](adr/ADR-038-instances-clients-and-remote-access.md) for the
decision and deferred implementation boundary.
