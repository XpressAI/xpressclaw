# Google Antigravity

Select **Google Antigravity** when creating an Agent. XpressClaw runs Google's
official CLI with `agy acp --process-per-session`. The image includes the pinned
CLI; installing it on the host is optional. See Google's
[ACP guide](https://antigravity.google/docs/cli/acp/) and
[authentication reference](https://antigravity.google/docs/install?tab=cli).

## Google account login

Open **Agent → Harness → Sign in**, or choose **Sign in** on a failed Task or
Conversation. XpressClaw starts Antigravity's advertised login flow inside the
dialog. Open its Google URL in your browser and paste the resulting code into
the dialog. Once it completes, XpressClaw reconnects the runner and offers a
task retry. Sending `/login` as a task message does not initiate authentication.

Keep **Use host login and harness configuration** enabled when you want to
share its configuration files. Host logins stored only in the OS keyring need
the in-app sign-in flow because the keyring is not part of the mounted files.
Antigravity's headless login stores its token at
`~/.gemini/antigravity-cli/antigravity-oauth-token`; the mounted directory retains
it for subsequent sessions. Disable host sharing for a separate Agent login.

See [Agent sign-in](agent-sign-in.md) for the shared authentication lifecycle.

## Gemini API key

Add `GEMINI_API_KEY` under **Harness → Harness environment** and save. In the same
home directory used by the runner, create or edit
`~/.gemini/antigravity-cli/settings.json`, preserving other settings:

```json
{
  "modelProvider": "gemini"
}
```

With host login enabled, edit that file on the host. Otherwise edit it in
**Files → Terminal**. Antigravity 1.5.5 requires this provider selection;
setting the environment variable alone leaves ACP sessions unauthenticated.
The key uses the existing runner environment path and is read by the CLI.
To return to Google account authentication, remove `modelProvider` and the
runner's API-key environment entry, then choose **Sign in** in XpressClaw.

## State and sessions

Host login mounts these directories read-write when they exist:

| Host path | Runner path | Contents |
| --- | --- | --- |
| `~/.gemini` | `/home/node/.gemini` | Login token, CLI settings, shared configuration, session history and artifacts |
| `~/.config/antigravity` | `/home/node/.config/antigravity` | Credential scope and plugin secrets |

Settings live under `.gemini/antigravity-cli/settings.json` and
`.gemini/config`; conversations and artifacts live under
`.gemini/antigravity-cli`. Mounted state survives container replacement.
When host login is disabled, these files remain in the Agent's retained
container and survive normal recovery with its other files. An explicit
fresh-environment rebuild uses the existing rebuild semantics; use explicit
writable mounts for these directories to keep an isolated login across such
rebuilds.

XpressClaw discovers the models and thinking levels reported by the connected
CLI. Task and Conversation sessions use the common ACP lifecycle, including
permissions, tool updates, cancellation, and load/resume. Model availability
comes from the selected account or provider.

The GitHub and coordination servers use ACP's stdio MCP transport. Selected
HTTP MCP servers use the CLI's per-session worker mode, which Google requires
for client-configured HTTP servers. The pinned CLI advertises stdio and HTTP;
use those transports for configured MCP servers.

## Images and checks

The built-in kind is `antigravity`, with `agy` as an alias. Standard images use
`xpressclaw-runner-antigravity`; host-engine variants use
`xpressclaw-runner-antigravity-docker`. Release builds select the immutable
runner revision through the existing version machinery.

The manifest pins the official GitHub release and each Linux archive's
SHA-256. The version updater follows that official release source. The ACP
Registry's separate Antigravity distribution does not replace the `agy` CLI.

```sh
bash scripts/build-runner-images.sh antigravity
node --test scripts/runner-versions.test.mjs harnesses/native/common/mcp-notifications.test.mjs
```

Each Antigravity image build exercises the official binary against local
Gemini and MCP fixtures: provider selection, model/thinking discovery,
GitHub tool discovery, scoped coordination calls, HTTP MCP headers and calls,
permission requests, command execution, session reload after process
recreation, and active cancellation. These checks use a fixture API key and
make no live model request.
