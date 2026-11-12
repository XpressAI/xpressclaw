# Sign in to an Agent

Open **Agent → Harness → Sign in**, or choose **Sign in** on an authentication
failure in a Task or Conversation. XpressClaw prepares the configured runner
and offers the authentication methods it advertises over ACP.

- Interactive login appears inside the dialog. XpressClaw starts the login
  command; follow the provider's instructions and open its links in your browser.
- Browser/device-code login presents the provider's URL and instructions.
  Complete the provider's flow, then confirm in XpressClaw.
- Provider forms and advertised API-key inputs appear in the same dialog.

After successful login, choose **Retry task** to resume failed task work. In a
Conversation, send the next message. Closing the dialog cancels an unfinished
login. A failed or interrupted login is not reported as successful.

Host configuration sharing remains optional. File-based host credentials can
be reused through the configured mounts. A host OS-keyring login is not made
available by mounting its configuration directory; use the in-app sign-in flow
to authorize the runner in that case.

## Runtime behavior

XpressClaw negotiates ACP terminal authentication and URL/form elicitation.
Terminal methods launch the configured Agent command with the advertised
additional arguments and environment. Each login owns a private terminal;
cancellation closes that terminal without closing ordinary Files terminals.
Built-in images include tmux, which provides this terminal isolation.

A zero exit status from a terminal login reconnects the Agent's ACP runtime.
Agent-managed methods use ACP `authenticate` and retain that authenticated
connection. Idle Conversation connections are refreshed after either flow.
The Agent owns credential persistence, using its existing writable mounts or
retained container files.

Sign-in holds the Agent's runtime maintenance boundary, so it cannot race
active task execution, recovery, or deletion. It creates no Tasks, work
attempts, model prompts, or probe conversations. Login output, API keys, and
elicitation responses are not written to task history. Application logging
excludes protocol libraries' raw payload logs, including at debug/trace levels.

The browser API uses the instance's normal session and same-origin checks.
Runner callbacks cannot initiate this user-facing authentication flow.

See the [ACP authentication flow](https://agentclientprotocol.com/rfds/auth-methods)
and [Antigravity setup](antigravity.md).
