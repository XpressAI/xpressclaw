# ADR-024: Android Device Control (Managed Emulator + adb_client)

## Status
Accepted

## Context

Commit `ae5fbc5` ("agentic android control via opt-in claude-sdk-android
harness") added a first cut of Android control: an `android-pilot` agent
template wired to the off-the-shelf `scrcpy-mcp` MCP server, auto-routed to a
heavier `claude-sdk-android` Docker image. It deferred this ADR.

Reviewing that approach surfaced three problems:

- **Vision-coordinate targeting is unreliable.** The bundled prompt told the
  agent "coordinates come from what you see in the screenshot — never guess,"
  but that *is* the guess: vision LLMs resize the image internally and emit
  coordinates at the displayed scale. We reproduced the miss independently —
  eyeballing the Photos icon gave `x≈560`, the real center (`uiautomator`) was
  `x=664`, a ~100px miss.
- **Implicit image swapping.** `image_for_agent()` string-sniffed an agent's
  MCP args for `"scrcpy-mcp"` and silently substituted a 60 MB-heavier image.
- **scrcpy version coupling.** scrcpy-mcp pins a v4.x server JAR; distro adb
  ships scrcpy 1.25, producing "server version 1.25 does not match client 4.0".

The product use case also sharpened: **let a user log into an Android device
and have an xpressclaw agent control it.** That forces two decisions the
original commit never recorded — how we *talk to* the device, and where the
device *comes from*.

### Control transport options

| Option | Notes |
|--------|-------|
| Shell out to `adb` | Matches codebase precedent (telegram/github hand-roll their interface; `commands/android.rs` shells `docker`). Requires the `adb` binary present. |
| `adb_client` crate (pure Rust) | No `adb` binary; typed API; talks to an adb server *or* directly to a device over USB/TCP. |
| `scrcpy-mcp` (original) | Off-the-shelf, vision-coordinate based; carries the targeting bug above. |

### Device-provider options (and the licensing constraint)

We **cannot redistribute** Google's `adb`/platform-tools or Google-API/Play
system images — the SDK license forbids it, and Play images need a commercial
OEM agreement. Only **AOSP** images (Apache-2.0) are redistributable, but they
lack Google Play / Google login.

| Provider | Ship? | Host | Google login |
|----------|-------|------|--------------|
| BYO real device / user's own emulator | only `adb_client` | any | yes (their device) |
| **Managed local emulator** (user installs SDK; we orchestrate) | only `adb_client` + orchestration | Win/Mac/Linux | yes (user pulls Google images under Google's license) |
| Containerized `redroid` (AOSP) | image is redistributable | **Linux only** | no (AOSP, no Play) |
| Cloud device farm | API client | n/a | provider-dependent; data leaves machine |

## Decision

Replace the `scrcpy-mcp` approach with an **opt-in feature** built on
`adb_client`, targeting elements through Android's built-in `uiautomator`
accessibility tree (which we consume, not reimplement), over two device
providers.

**1. Control transport: the `adb_client` crate**, behind a new `android` Cargo
feature that is **off by default**. Default builds don't pull `adb_client`'s
`rustls`/`rsa`/`image` tree, preserving the ~12 MB single-binary story
(opt-in-dependency precedent: ADR-023). A spike validated the full
screenshot → find-element → tap → verify loop in pure Rust against an emulator,
both via the adb server *and* via a direct TCP connection to `adbd` with the
adb server killed (no `adb` binary, no server).

**2. Element targeting via the accessibility tree, not vision coordinates.**
Resolve targets through `uiautomator dump` bounds (`tap_text`/`find_element`) —
the fix for the targeting bug above. A spike (`android-vision-spike/`) confirmed
the choice against `uiautomator` ground truth: vision-coord tapping scored ~4/5
on frontier models but 0–1/5 on the in-house models this deployment prioritizes
(one is text-only), and downscaling the image made it worse (coords scale back
up, amplifying error). Accessibility bounds are exact on *any* model. Vision is
deferred as a fallback for canvas/game screens the tree can't read.

**3. Two device providers behind one provider-agnostic control layer.** Both
required; `adb_client` connects to whatever `adbd` is present, so providers
differ only in provisioning/discovery, not control.
- **BYO** — the user connects a real phone (USB/wireless debugging) or their
  own running emulator; xpressclaw discovers and controls it. Ships nothing but
  `adb_client`.
- **Managed emulator** — the user installs the SDK; xpressclaw orchestrates
  `sdkmanager` → `avdmanager` → `emulator` → `adb_client`, so the user pulls
  Google's images themselves (no redistribution exposure). Default image is
  `google_apis_playstore` (Store + Google login), defined once as
  `android::sdk::DEFAULT_SYSTEM_IMAGE`; the cost is no `adb root`, fine since
  tap/screenshot/uiautomator need none.

`redroid` (AOSP, Linux) is recorded as a future third provider. We never bundle
Google's emulator or images.

**4. Agent-facing layer: our own MCP tool** — `screenshot`,
`find_element`/`tap_text`, `tap`, `swipe`, `input_text`, `key_event` —
superseding `scrcpy-mcp` and its implicit image-swap (MCP per ADR-005).

**5. Human login view.** The emulator's own window is the login surface
initially; an embedded stream (scrcpy/noVNC) in the web UI is a follow-up.
Logins persist via an AVD snapshot.

**6. Surfaced as a "device-link" connector** in the Connectors grid for
discoverability, even though control is *not* an event source/sink. The
`AndroidConnector` carries only the adb target (serial/tcp); its
`validate_config`/`health` probe reachability via `adb_client` (the analog of
Telegram's `getMe`), emit no events, and reject `send`. A CLI
`xpressclaw android doctor` reports the managed-emulator SDK preflight
(emulator, images, AVDs, accel) via `android::sdk::detect()`.

## Consequences

### Positive
- No redistribution/licensing exposure; the only added binary footprint is
  `adb_client`, and only under `--features android`.
- Cross-platform (Windows/macOS/Linux) via the local emulator.
- Reliable element targeting (accessibility tree) instead of vision guesses.
- Removes the implicit image-swap and scrcpy version coupling.

### Negative
- Users must install the Android SDK separately (a few GB) — same trade-off
  ADR-023 accepted for Ollama.
- **Google-login integrity risk:** emulators can trip Play Integrity checks;
  AOSP/`redroid` fail certification outright. Fine for non-Google app logins; a
  real risk if the accounts are Google.
- We still shell out to the `emulator` binary to *launch* a device (`adb_client`
  cannot start one) — but that binary is user-installed, not shipped.

### Risks
- Emulator hardware acceleration (WHPX/KVM/HAXM) must be present on the host.
- The `android` feature must be plumbed consistently across `core`, `cli`,
  `server`, and `tauri` (comprehensive-changes rule).

## Related ADRs
- ADR-003: Container Isolation (redroid would slot in as a provider here)
- ADR-005: MCP Tool System (the agent-facing control tool speaks MCP)
- ADR-023: Ollama-only Local Inference (precedent for opt-in external deps)
