# Android Control — POC Setup

This is the minimal setup for driving an Android device or emulator from
an xpressclaw agent. See `docs/adr/ADR-024-android-control.md` for the
design rationale and what's deliberately out of scope.

## What you get

A new `android-pilot` agent template that talks to a host-side
[`scrcpy-mcp`](https://github.com/JuanCF/scrcpy-mcp) server. The agent
can list devices, screenshot, tap, swipe, type, launch apps, dump UI
trees, and more (34 tools total).

## Host prerequisites

You need all of these on the machine running xpressclaw:

- **Node.js 22+** — `node --version`. `scrcpy-mcp` is shipped via npm.
- **adb** — from Android Platform Tools. `adb version` should print
  something. On Linux: `apt install android-tools-adb` or grab the
  platform-tools zip from Google. On macOS: `brew install android-platform-tools`.
- **scrcpy** — strongly recommended (10-50x faster screenshots). `scrcpy --version`.
  Linux: `apt install scrcpy`. macOS: `brew install scrcpy`.
- **An Android target.** For this POC: an Android Studio AVD running in
  the foreground. Real devices over USB also work — just plug in,
  enable USB debugging, accept the trust prompt.

Quick sanity check before you wire anything into xpressclaw:

```bash
adb devices
# List of devices attached
# emulator-5554   device

npx -y scrcpy-mcp --help
# (or the MCP server starts and waits on stdin)
```

If both work, you're ready.

## Why isolation must be off

xpressclaw normally runs MCP servers inside the agent's Docker container.
For this POC we run them on the host so the server can reach the host
`adb` daemon and the running emulator without any port forwarding gymnastics.

In your `xpressclaw.yaml`:

```yaml
system:
  isolation: none   # required for the android-pilot agent
```

This relaxes the safety boundary for *all* agents in this workspace. If
you want isolation back for your other agents, run them in a separate
workspace.

## Configure the agent

`xpressclaw init` and pick the **Android Pilot** template from the setup
wizard, or hand-edit `xpressclaw.yaml`:

```yaml
agents:
  - name: pilot
    backend: claude-sdk
    role: |
      You are an Android pilot. (see the template's role for the full prompt)
    default_mcp_servers:
      scrcpy:
        type: stdio
        command: npx
        args: ["-y", "scrcpy-mcp"]
    llm:
      provider: anthropic            # vision-capable model required
      model: claude-sonnet-4-6
      api_key: ${ANTHROPIC_API_KEY}
```

A local vision-capable model via Ollama works too (Qwen2.5-VL,
LLaVA-style) — set `provider: ollama` and the relevant tag.

## Try it

1. Start your AVD.
2. `xpressclaw up`.
3. Open the chat for the `pilot` agent.
4. Ask: **"Open the Android Settings app and screenshot the home of Settings."**

Expected tool-call sequence:

1. `device_list` — confirms a device is connected.
2. `start_session` — opens the fast scrcpy frame buffer.
3. `app_start` with `com.android.settings` (or `app_list` first to find it).
4. `screenshot` — returns the image.

If the agent loops or the screenshot is blank, see *Troubleshooting* below.

## Troubleshooting

**`adb devices` shows the emulator as `offline`** — wait ~10 seconds after
boot. The kernel is up before adb is. `adb kill-server && adb start-server`
if it stays offline.

**`npx -y scrcpy-mcp` hangs or fails** — make sure Node is 22+. Some
distros default to 18 or 20. Use `nvm install 22` and retry.

**Agent can't see screenshots** — confirm the LLM is vision-capable.
Claude Sonnet 4+, GPT-4o, and Qwen2.5-VL all work. Plain-text local models
will receive the image as an opaque blob and act blind.

**Multiple devices connected** — `scrcpy-mcp` defaults to "the single
device if there's only one." With several attached, the agent needs to
pass `serial` in every call. Easiest fix for the POC: unplug everything
except your test target.

**Slow screenshots (~500ms)** — that's the ADB fallback. Install `scrcpy`
on the host and the server switches to the ~33ms frame-buffer path
automatically.

## What's not in this POC

See ADR-024 for the followups: Dockerized emulator profile, dedicated
`xpressclaw android` CLI, live screen viewer in the dashboard, native
Rust MCP server.
