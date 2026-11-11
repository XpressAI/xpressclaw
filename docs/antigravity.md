# Google Antigravity

Select **Google Antigravity** when creating an Agent. XpressClaw runs Google's
official CLI with `agy acp --process-per-session`. The image includes the pinned
CLI; installing it on the host is optional. See Google's
[ACP guide](https://antigravity.google/docs/cli/acp/) and
[authentication reference](https://antigravity.google/docs/install?tab=cli).

## Google account login

With a host installation, run `agy acp --login` and leave **Use my existing
Google Antigravity login** enabled. The command prints a Google authorization
URL and accepts the resulting code in the terminal.

To sign in inside the Agent, disable host login, create the Agent, open
**Files → Terminal**, and run:

```sh
agy acp --login
```

This also works when the control plane runs on another machine. Antigravity's
headless Linux login stores its token at
`~/.gemini/antigravity-cli/antigravity-oauth-token`. A host desktop login held
only in an OS keyring needs a separate login inside the container.

## Gemini API key

Add `GEMINI_API_KEY` using **Runner → Environment** and save. In the same
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
runner's API-key environment entry, then sign in with `agy acp --login`.

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
