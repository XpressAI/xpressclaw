# Control center

Open **Control center** from the XpressClaw brand in the top-left corner. The
page is designed to stay open while Agents work: it shows the whole instance
or one Project, with `1h`, `24h`, and `7d` windows.

## Server resources

The summary cards show **CPU**, **Memory**, **Disk**, and **Needs you**.
CPU, memory, and disk describe the machine running the XpressClaw server and
always cover all Projects, independently of the selected time window. They
refresh every five seconds while the dashboard is visible, including when no
Agent is working. A container or VM installation can only report the resources
visible to that environment; these are not per-Agent measurements or limits
for a remote Docker host.

- CPU is aggregate utilization across logical CPUs between samples. The first
  sample, including after a long absence, is labeled **Sampling** until a second
  measurement is available.
- Memory shows used and available capacity. Reclaimable memory is counted as
  available, so an OS file cache does not look like exhausted RAM.
- Disk monitors the filesystems containing the configured instance data and
  workspace directories. Locations on the same volume share a reading; separate
  volumes are listed individually. Used and available bytes and utilization
  help reveal storage pressure. Unavailable readings are not presented as zero.
- Needs you retains the selected Project scope: Tasks waiting for input or
  blocked, plus failed Conversation responses. The attention rail links to each.

Failed resource requests retain the previous reading with an explicit stale
label and a retry button. The server shares a cached sampler across dashboards,
performs OS reads off its async executor, and does not scan processes, run GPU
commands, sleep during requests, or acquire Agent lifecycle locks.

## Activity and token counts

Active now retains the current Agent count and queued/running work, with queued
and working durations labeled separately. The Activity signal has three views:

- **Context** is ACP context-window occupancy. The graph is not summed to
  estimate consumption. Alongside it, **Reported tokens** shows total, input,
  output, cache read/write, and reasoning counts in the selected Project/window.
- **Tools** is canonical tool-call volume. Progress/completion updates do not
  increase the count again.
- **Code** is normalized Git additions and deletions attributed to each response.

Token reports are stored durably for each completed ACP prompt request; one
Task/attempt can contain multiple requests. They are assigned to the time the
response is received, so a request crossing a time-window boundary contributes
its entire report to the window containing its response. Running requests and
requests interrupted by a control-plane crash have no final report. Missing
reports and unsupported accounting are counted separately from reported zeros.
Recording starts at this database upgrade; historical context values and legacy
character-based budget estimates are not backfilled as token counts.

ACP's [end-turn token format](https://agentclientprotocol.com/rfds/end-turn-token-usage)
is still a draft with ambiguous per-turn versus cumulative semantics. Currently,
XpressClaw aggregates the built-in Claude and Codex bridges' per-response
`PromptResponse.usage` reports. Other harnesses' reports are marked unsupported
until their accounting convention is verified, avoiding silent double counting
of cumulative session totals. This limitation is visible in the dashboard.
Even supported harnesses can omit usage. These are harness-reported counts, not
billing guarantees; model calls/subagents omitted by the harness remain omitted.
The reported `totalTokens` is preserved directly: cache and reasoning fields may
already be included in other categories and are never added to the total again.
Optional categories show only the values actually provided, or a dash when none
were supplied. No model prices or estimated costs are invented here.

For Git metrics, XpressClaw records the repository commit and
`git diff --numstat` state at the start of the response, then samples at
debounced tool boundaries and at the end. This preserves attribution when an
Agent commits during its turn while subtracting dirty changes that existed
before the turn. If a later snapshot reverts added lines or restores deleted
lines, the chart records that as reverse activity instead of retaining stale
line counts. Binary or untracked files, and attribution that overlaps
pre-existing dirty files, make the result **Partial**. A missing workspace,
non-Git directory, or repository without a baseline commit is reported as
unavailable rather than as zero.

## Live updates and privacy

The live feed displays **Agent Updates and responses** from Tasks and
Conversations. User messages, tool starts, generic runner progress, and lifecycle
notifications do not crowd out Agent text. Status events still update Active now
and the attention rail. Feed filtering happens before server pagination. Non-displayable deletion
markers are retained so refreshed pages can evict older deleted messages; the
browser also filters incoming stream events while processing deletions and
refreshing state. Adjacent genuine Updates are not throttled away.

The page loads one bounded snapshot and opens one instance-wide event stream;
it does not subscribe separately to every Task or Conversation. Events have a
durable SQLite cursor, so a reconnect can replay missed activity after a UI or
control-plane restart. Older feed rows are loaded only when you request them.
Resource refreshes use a separate lightweight endpoint and survive work-filter
changes; hidden dashboards pause that polling.

The global feed stores and displays short plain-text summaries. It does not
copy raw tool arguments, terminal output, hidden reasoning, credentials from
runner configuration, diffs, or unbounded message bodies into dashboard
telemetry. Message text is rendered as literal text, so HTML-like input cannot
execute. The internal tool-event stream retains only bounded activity categories.

Recent dashboard events, metric points, and token reports are retained for eight
days, with a 20,000-row cap on events. Tasks, Conversations, and their full
histories keep their existing lifecycle and retention; these tables are only a
bounded index for the control-center view.
