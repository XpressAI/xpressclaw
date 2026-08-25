# Control center

Open **Control center** from the XpressClaw brand in the top-left corner. The
page is designed to stay open while Agents work: it shows the whole instance
or one Project, with `1h`, `24h`, and `7d` windows.

## What it shows

- **Working Agents** counts Agents whose current Task or Conversation response
  is actively preparing or running.
- **Active work** includes queued and actively running Task attempts and
  Conversation turns. Queued time and working time are labeled separately.
- **Needs you** counts current Tasks waiting for input or blocked, plus failed
  Conversation responses. The attention rail links to each affected item.
- **Tool calls** counts canonical ACP tool-call starts in the selected window.
  Tool progress and completion updates do not increase the count again.

The Activity signal is one chart with three views:

- **Context** is ACP context-window occupancy. It is not billing-grade input or
  output token accounting and is deliberately labeled as context usage.
- **Tools** is canonical tool-call volume.
- **Code** is normalized Git additions and deletions attributed to each
  response turn.

For Git metrics, XpressClaw records the repository commit and
`git diff --numstat` state at the start of the response, then samples at
debounced tool boundaries and at the end. This preserves attribution when an
Agent commits during its turn while subtracting dirty changes that existed
before the turn. Binary or untracked files, and attribution that overlaps
pre-existing dirty files, make the result **Partial**. A missing workspace,
non-Git directory, or repository without a baseline commit is reported as
unavailable rather than as zero.

## Live updates and privacy

The page loads one bounded snapshot and opens one instance-wide event stream;
it does not subscribe separately to every Task or Conversation. Events have a
durable SQLite cursor, so a reconnect can replay missed activity after a UI or
control-plane restart. Older feed rows are loaded only when you request them.

The global feed stores and displays short plain-text summaries. It does not
copy raw tool arguments, terminal output, hidden reasoning, credentials from
runner configuration, diffs, or unbounded message bodies into dashboard
telemetry. Message text is rendered as literal text at this boundary, so
HTML-like input cannot execute. Tool rows use a bounded activity category such
as “Reading workspace data” or “Running a command,” never the command, raw
input, or output itself.

Recent dashboard events and metric points are retained for eight days, with a
20,000-row cap on events. Tasks, Conversations, and their full histories keep
their existing lifecycle and retention; the dashboard tables are only a
bounded index for the control-center view.
