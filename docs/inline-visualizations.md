# Inline visualizations

Codex's bundled Visualize skill can return an interactive HTML fragment in a
Task or Project Conversation. XpressClaw recognizes the exact visualization
reference in a final Agent reply and shows the result in place. Normal and
wide layouts are supported, and any visual can be expanded for a larger or
mobile-friendly view.

The generated file must be an HTML fragment of at most 1 MiB inside that
Agent's writable workspace or an explicitly configured writable volume. When
the final reply arrives, XpressClaw copies the fragment into its own durable
message storage. Reloading the page or replacing the Agent container therefore
does not depend on the original file still existing. Missing, invalid,
oversized, or out-of-workspace references remain visible as an unavailable
card with a useful reason.

## Security boundary

Visualization HTML is untrusted. It is never inserted into the XpressClaw DOM.
The viewer loads a capability-protected copy in a sandboxed, opaque-origin
iframe with a restrictive content security policy:

- scripts may run inside the iframe, but it receives no same-origin access;
- host cookies, storage, XpressClaw APIs, forms, popups, objects, workers,
  top-level navigation, and network connections are unavailable;
- static scripts, styles, fonts, and images are limited to the CDN origins in
  the Codex Visualize contract; and
- referrers are disabled.

The only host bridge is
`window.openai.sendFollowUpMessage({ prompt, title })`. XpressClaw validates the
request and shows the exact prompt in a confirmation dialog. Nothing is sent
to the originating Task or Conversation until the user confirms it.

Visualization references in user or system messages are always inert text.
Malformed, escaped, or lookalike references are also rendered through the
normal raw-HTML-safe Markdown path.
