# ADR-040: Optional Authentication and Remote Instance Profiles

## Status

Accepted

## Context and threat model

An XpressClaw instance can edit repositories and invoke credentials available
to its Agents. The protected assets are instance APIs, Project and message
data, files/artifacts, terminals, streams, workflow controls, and assigned
Agent capabilities. Relevant attackers are another browser origin, a device
with network reachability, a brute-force login client, untrusted forwarded
headers, leaked logs/diagnostics, and remote web content attempting to read a
Desktop keychain credential.

The operator may deliberately trust a LAN or private tailnet. Requiring app
authentication in that topology would make the common direct Tailscale flow
needlessly cumbersome and would still not encrypt transport. Conversely,
presenting optional app authentication as TLS would create false confidence.
Runner callbacks already have a separate per-process capability and must not
be coupled to browser sessions.

## Decision

- Loopback with authentication off remains the migration-safe default.
- Non-loopback with authentication off is allowed only after an explicit,
  persisted operator acknowledgement (or the existing one-start CLI flag).
- App authentication is optional. Passwords use Argon2id and only the verifier
  is persisted in a restricted instance secret file. With no password, a
  cryptographically random token is generated each start and delivered once
  through an operator-owned foreground channel.
- Every user-facing API, mutation, stream, WebSocket upgrade, proxy, terminal,
  attachment, and artifact route shares an authentication boundary. Only
  static bootstrap/login content and minimal health/auth bootstrap responses
  are public. Runner callbacks retain their independent capability boundary.
- Browser sessions are opaque, process-local, expiring HttpOnly SameSite
  cookies. State-changing requests require a per-session CSRF header. Login
  creates a new session, is rate-limited by the direct peer address, and uses
  constant-time secret comparison. A loopback reverse proxy may supply the
  final observed `X-Forwarded-For` address for per-client throttling; forwarded
  identity from non-loopback peers is ignored. Restart or credential/mode
  change revokes sessions.
- Proxy headers are not trusted to decide cookie security or authorization.
  An HTTPS browser Origin adds the Secure cookie attribute for externally
  terminated TLS, while direct trusted HTTP remains usable. TLS, HTTP
  redirection, and HSTS remain the operator's responsibility.
- Desktop keeps one automatic local profile and any number of explicit remote
  profiles. Non-secret profile metadata is local to Desktop; credentials use
  the OS keychain. Desktop pins a long-lived Ed25519 instance public key and
  verifies a fresh signed challenge before granting local plugin permissions;
  the private key remains in restricted instance secret storage. Desktop
  releases a saved password or startup token only through a one-use channel
  authenticated by that Ed25519 key: signed ephemeral X25519 keys feed
  HKDF-SHA256 and directional ChaCha20-Poly1305 request/response keys. A relay
  may forward the exchange but cannot decrypt the long-lived credential from
  it. On first local startup, the bundled child announces its public key over
  inherited stdout only after it owns both listeners. Automatic keychain login
  additionally requires an HTTPS remote origin or the exact local HTTP identity
  announced by that listener-owning child. Desktop does not submit a saved
  credential or install a session for an HTTP remote origin, because a relay
  could receive and reuse a cookie scoped to that unproved endpoint; direct
  trusted-tailnet HTTP remains available through the browser login form. For an
  eligible origin, the requested browser session is returned only inside the
  encrypted channel; native Desktop installs its HttpOnly cookie and returns
  only success/failure to web content. Passwords, startup tokens, session
  values, and redeemable bearer tickets do not cross the native command
  boundary.
- Tauri application commands have an explicit ACL. Bundled local content gets
  the existing local command set; only the exact selected remote origin gets
  the narrower profile/login command set. Other remote origins cannot invoke
  Desktop commands or inherit a wildcard capability.
- One selected profile applies to all Desktop windows. Switching closes
  secondary windows. This explicit constraint prevents mixed-instance state
  without pretending per-window isolation exists.

## Consequences

Direct no-auth tailnet operation remains simple and deliberate. Operators who
need defense in depth can enable a password/token without changing the network
topology. Neither mode claims transport confidentiality. Saved listener/auth
changes are distinguished from effective process values until restart, while
password changes revoke sessions immediately. Existing `--bind` and
`--allow-insecure-remote` scripts remain valid, and explicit CLI listener
values continue to override saved configuration.

The Desktop credential channel protects the keychain secret from a relaying
or replacement HTTP endpoint; it is not a substitute for TLS and does not
authenticate or encrypt the subsequent browser session. Operators who need
transport integrity or protection against an active relay for all application
traffic must use HTTPS, an SSH tunnel, or a trusted tailnet path.
