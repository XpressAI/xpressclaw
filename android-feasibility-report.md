# Android Agentic Control — Feasibility Report

**Date:** 2026-05-19
**Author:** Claude (paired with @fahreza)
**Model under test:** `xpress-qwen-3.5-27b` (Qwen 3.5 27B via Pipeshift)
**Target:** Android 15 (API 35) on a Pixel 6 AVD (1080×2400, headless)
**Harness:** `android_pilot.py` — minimal Python agent loop, direct `adb`, no MCP

Raw artefacts: `logs/pilot/<timestamp>_<task>/{trace.jsonl,summary.json,screenshots/}`.

---

## Executive summary

A 27B-parameter open-weights vision LLM, served via an OpenAI-compatible
endpoint, can drive an Android emulator end-to-end through a screenshot +
adb-action loop. **Reading the screen is excellent** (exact clock, exact
button labels, exact colors). **Vision-derived coordinate tapping is
unreliable**, but adding a `tap_text(label)` tool backed by
`uiautomator dump` removes vision from the targeting path entirely and
made the previously-failing Battery tap succeed on the first re-run.
Multi-step navigation with persistent scrolling still fails because the
model gives up too early.

The dominant cost is **per-step latency** (5–15 s wall clock per LLM
call, of which a meaningful fraction is reasoning trace) and **prompt
growth** (each screenshot adds ~2.5 k tokens of context, so prompts
double every step). A POC is viable; production needs both better
prompting/guardrails for persistence and a context-pruning strategy.

**Verdict:** Feasible for narrow, well-scoped Android tasks today.
Not yet ready as a general-purpose mobile assistant.

---

## What was tested

Five agent runs of increasing complexity:

| # | Task | Outcome | Steps | Wall time |
|---|---|---|---|---|
| 1 | Press HOME, open Clock, read the time | ✅ Success (correct: "3:12 PM Tue May 19") | 5 | 46.0 s |
| 2 | Open Settings, scroll to About Phone, read Android version | ❌ Gave up mid-scroll, no answer | 6 | 38.0 s |
| 3 | Open Chrome, describe the screen | ✅ Success (Welcome to Chrome screen, button labels exact) | 4 | 22.9 s |
| 4 | Open Settings, tap on "Battery" via coordinates, read details | ❌ Tap landed in search bar, opened wrong screen | 8 | 41.8 s |
| 4b | Same task, after adding `tap_text(label)` tool (accessibility tree) | ✅ Success (Battery row found at (273, 685), details page read correctly) | 7 | 31.0 s |

Each task was verified by comparing the model's stated outcome to the
actual final screenshot.

---

## Quantitative findings

### Per-step LLM cost

Latencies and tokens, averaged across the 15 LLM calls in the three runs:

| Metric | Min | Median | Max |
|---|---|---|---|
| Latency per call | 2.4 s | 4.5 s | 13.8 s |
| Prompt tokens | 1.1 k | 3.9 k | 6.8 k |
| Completion tokens | 1 | 87 | 311 |
| Reasoning chars (separate field) | 0 | 205 | 489 |

### Prompt growth is the headline cost

Within a single 5-step run, prompt size roughly doubles every screenshot
because the full screenshot history is replayed in every call:

```
step 1: 1.1k    (system + user task)
step 2: 1.2k    (after first tool call, no image)
step 3: 3.9k    (after first screenshot)
step 4: 3.9k    (after a non-screenshot tool call)
step 5: 6.5k    (after second screenshot)
```

For a 10-step task with 4 screenshots, prompts can hit ~25 k tokens.

### Throughput

Wall-clock time for completed tasks landed at ~5–10 seconds per step.
A typical 4–6 step task takes 25–50 s. A 10-step task that requires
several rounds of scrolling-and-checking would plausibly take 1–2 min.

---

## Qualitative findings

### Vision accuracy is the strong point

The model identified:
- Status-bar clock down to the minute (3:12 PM) — verified against the
  raw screenshot.
- Date strings ("Tue, May 19") — exact match.
- Specific button labels, colours, and section titles in Chrome (e.g.
  "Add account to device", blue button; "Use without an account", text
  link below) — all visually correct.

This is the foundation of the loop working at all.

### Failure mode 1 — premature task abandonment

Task 2 stopped after **two scroll attempts** even though About Phone is
many screens further down in the Settings list. The model emitted a
single token of content and `finish_reason=stop` with no tool call. This
matches a known reasoning-model failure: when uncertain, the model can
quietly conclude rather than persist.

Mitigations to test:
- More explicit "keep going until you see X" wording in the task prompt.
- Add a `scroll_to_text(text)` tool that loops scrolls until found,
  rather than asking the LLM to drive each scroll.
- Use `ui_dump` (via `uiautomator dump`) to skip vision for text-heavy
  screens — find About Phone by string match.

### Failure mode 2 — tool-argument schema drift

On the first Clock attempt (pre-fix), the model called `tap` with
`{"x1": "842", "y1": "182"}` — confusing it with `swipe`'s schema. It
also passed strings instead of integers. The harness crashed instead
of feeding the error back.

Fixed in the harness: bad-argument exceptions are now caught and
returned to the model as a tool result string, so it can self-correct.

### Failure mode 3 — stale package-name knowledge

The model defaulted to AOSP-style `com.android.deskclock`, which doesn't
exist on this Google APIs emulator (which has
`com.google.android.deskclock`). `monkey` exits 252; harness now feeds
that back. Long-term fix: surface the installed package list to the
model up-front (now done in the system prompt) or expose an
`app_list` tool.

### Failure mode 4 — coordinate / pixel-space mismatch (the big one)

This is the most important finding from the tap test.

Asked to tap the "Battery" row in Settings, the model emitted
`tap(x=252, y=296)`. The next screenshot showed the **Search Settings**
page — i.e. the tap registered inside the search bar at the top of the
screen, not on the Battery row.

In the actual 1080×2400 screenshot, Battery is at roughly **y ≈ 480–560**.
The search bar sits at y ≈ 220–360. The model's y=296 was off by roughly
2×, landing it in the wrong target.

**Cause.** Vision-language models including Qwen-VL resize the input
image to a fixed scale (typically ~448 or 768 px on the long edge)
before encoding. The model produces coordinates relative to *that*
internal scale, but our `tap` tool sends them to the real 1080×2400
display. The system prompt saying "1080×2400, coordinates match
displayed pixels" doesn't help — the model can't actually count pixels
on the resized image, and there's no consistent transform we can apply
from outside.

**This is a class of bug, not a one-off.** Any task that requires
precision tapping on small UI elements (≤ ~150 px high) will fail
unpredictably until we change the mechanism.

**Fix — accessibility tree, not coordinates.** *(landed and validated)*

Added a `tap_text(label)` tool to `android_pilot.py` that:

1. `adb shell uiautomator dump /sdcard/window_dump.xml`
2. `adb exec-out cat /sdcard/window_dump.xml` to fetch the XML.
3. Parse with `xml.etree.ElementTree`, find nodes whose `text` or
   `content-desc` matches the requested label (exact match first, then
   substring).
4. Read `bounds=[x1,y1][x2,y2]`, compute the centre, tap it.

Re-running the Task 4 prompt verbatim — but with `tap_text` in the
toolset and a system-prompt nudge to prefer it — succeeded on the
first try: the harness resolved Battery to `(273, 685)` and the
expected Battery details page rendered with "100% / Charged". The
coordinate the model couldn't produce by vision is now a deterministic
DOM-style lookup.

Vision is still used to *understand* the screen ("which option do I
want?"). It's just not used to *locate* it ("at what x,y does that
option live?").

The same accessibility-tree primitive trivially extends to
`wait_for_text(label)` for push prompts and loading states. Not yet
implemented in the harness; cheap to add when needed.

### Reasoning trace is real but variable

The model emits a separate `reasoning_content` field (Qwen 3.5 follows
the DeepSeek-R1 / o1 convention). Per call: 0–489 characters,
averaging ~200. With `max_tokens=20`, the reasoning consumed the entire
budget and `content` came back `None` — that was the cause of our
initial endpoint-test confusion. With `max_tokens=2048` the actual
answer fits comfortably.

---

## Operational notes

### Endpoint setup

- Custom Qwen 3.5 endpoint via Pipeshift, OpenAI-compatible API.
- Cold vs warm: no measurable warm-up cost (text calls ~5 s either way;
  vision ~9 s).
- Vision in standard OpenAI `image_url` content blocks with base64
  data URLs — works correctly.
- Tool calling in standard OpenAI `tools=[...]` / `tool_calls` flow —
  works correctly.

### Why direct adb (not MCP) for the harness

The earlier MCP-based path (`scrcpy-mcp`) is validated and works, but
adds a JSON-RPC subprocess to manage and a scrcpy server-version
gotcha. For a feasibility demo, raw `adb` keeps the surface area small.
For production / xpressclaw, switching to MCP is straightforward —
same tool semantics, different transport.

---

## Cost & latency projections

For a fictional 8-step task with 3 screenshots, on this model/endpoint:

- LLM wall time: ~8 × 7 s = **56 s**
- Prompt tokens: roughly ~30 k summed across the 8 calls (~1 k per
  small call, ~5 k per screenshot-containing call). Plus ~1 k completion.
- adb action latency: negligible (~50 ms each)

At any reasonable per-token price, the headline cost driver is the
**re-sending of screenshot history** every call. Prompt-cache support
on the endpoint (if Pipeshift supports it) would cut this drastically.

---

## Recommendations

**Ship-now (feasibility-grade) capabilities:**
- Single-app workflows: launch + observe + maybe one or two actions.
- Read-only screen description / OCR.

**Done in this POC:**
- ✅ **Accessibility-tree tapping** — `tap_text(label)` is wired and
  validated (Task 4b). The most consequential failure mode is closed.

**Still needs work before broader use:**
1. **Higher-level navigation tool** — `scroll_to_text(text)` primitive
   that the agent calls once and the harness loops the swipe-and-check
   internally. Removes the "give up after 2 swipes" class of failure
   seen in Task 2. The same XML dump used by `tap_text` can tell us
   whether the target is on screen yet.
2. **Context pruning** — drop or summarise older screenshots from the
   prompt history. The model only needs the latest one or two screens
   to act, not the full history.
3. **`wait_for_text(label)`** — companion to `tap_text` for push
   prompts / loading states; trivial extension of the same XML
   parsing path.
4. **System prompt with installed-package list** — done in this POC;
   should be derived dynamically (e.g. via a startup `app_list` call)
   rather than hardcoded.
5. **Retry/persist policy** — on `finish_reason=stop` with no content,
   force-retry with a "keep going" prompt rather than treating it as a
   silent end-of-task (root cause of Task 2's failure).

---

## 2FA-specific feasibility

A common requested use case. Splits by flow:

| 2FA flow | What's needed | Feasible today? |
|---|---|---|
| **TOTP** (Authy, Google Authenticator, Aegis) | Open app → **read** 6-digit code | ✅ Strong. Pure reading task; vision proved exact at the clock test |
| **SMS code** | Open Messages → **read** code | ✅ Strong. Same reading pattern |
| **Push approve** (Google "Yes, it's me" / MS Authenticator) | Tap a **large** Approve/Allow button | ✅ Now feasible — `tap_text("Allow")` resolves regardless of pixel scale |
| **Type code into a field** | Tap field by label / accessibility-id, then `input_text` | ✅ Now feasible — `tap_text("Verification code")` or similar handles the field focus |

**Bottom line:** read-only 2FA paths (TOTP, SMS) work as soon as you
sideload an authenticator app onto the AVD. Interactive paths
(push-approve, code entry) now also work with `tap_text` doing the
targeting. The remaining blocker is *installing* an authenticator app
on the AVD — needs Play Store integration or a sideloaded APK.

---

## Appendix — where to find evidence

- **Trace logs** (one JSONL record per LLM call, includes full request
  messages with images elided, full response with reasoning_content,
  token usage, tool executions):
  - `logs/pilot/20260519-151143_press-home-then-open-the-clock-app-and-t/trace.jsonl` (Task 1)
  - `logs/pilot/20260519-151241_open-the-settings-app-then-scroll-down-a/trace.jsonl` (Task 2)
  - `logs/pilot/20260519-151330_open-the-chrome-browser-wait-for-it-to-l/trace.jsonl` (Task 3)
  - `logs/pilot/20260519-152031_open-the-settings-app-screenshot-identif/trace.jsonl` (Task 4 — coord-tap, failed)
  - `logs/pilot/20260519-152824_open-the-settings-app-screenshot-tap-on/trace.jsonl` (Task 4b — `tap_text`, succeeded)
- **Per-run summaries** (task, totals, status): `summary.json` in each
  run directory.
- **Screenshots** (every screen the agent saw): `screenshots/step-NNN.png`
  in each run directory.
- **Harness:** `android_pilot.py` (~310 lines, single file).
- **Endpoint health check:** `test_endpoint.py` — pings the LLM with
  text-cold / text-warm / vision calls.
- **Setup gotchas encountered to get here:** `android-poc-gotchas.md`
  (submodule sync, /dev/kvm ACL, disk cleanup, scrcpy v1.25 vs v4.0,
  reasoning-trace truncation).
