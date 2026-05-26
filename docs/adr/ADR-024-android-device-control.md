# ADR-024: Android Device Control (Managed Emulator + adb_client)

## Status
Proposed

## Context

Commit `ae5fbc5` ("agentic android control via opt-in claude-sdk-android
harness") added a first cut of Android control: an `android-pilot` agent
template wired to the off-the-shelf `scrcpy-mcp` MCP server, auto-routed to
a heavier `claude-sdk-android` Docker image. It deferred this ADR ("ADR-024
will land after the remaining implementation phases settle").

Reviewing that approach surfaced several problems:

- **Vision-coordinate targeting is unreliable.** The bundled prompt told the
  agent "coordinates come from what you see in the screenshot — never guess,"
  but that *is* the guess: vision LLMs resize the image internally and emit
  coordinates at the displayed scale. The commit's own POC hit this (a tap at
  `(252,296)` for a row at `y≈480`). We reproduced it independently: eyeballing
  the Photos icon gave `x≈560`, but the real center (from `uiautomator`) was
  `x=664` — a ~100px miss.
- **Implicit image swapping.** `image_for_agent()` string-sniffed an agent's
  MCP args for `"scrcpy-mcp"` and silently substituted a 60 MB-heavier image —
  hidden coupling between an arg substring and infra provisioning.
- **scrcpy version coupling.** scrcpy-mcp pins a v4.x server JAR; distro adb
  ships scrcpy 1.25, producing "server version 1.25 does not match client 4.0".

The product use case also sharpened: **let a user log into an Android device
and have an xpressclaw agent control it.** That makes the device part of the
product, forcing two decisions the original commit never recorded — how we
*talk to* the device, and where the device *comes from*.

### Control transport options

| Option | Notes |
|--------|-------|
| Shell out to `adb` | Matches codebase precedent (telegram/github hand-roll their interface; the existing `commands/android.rs` shells `docker`). Requires the `adb` binary present. |
| `adb_client` crate (pure Rust) | No `adb` binary needed; typed API; can talk to an adb server *or* directly to a device over USB/TCP. |
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

1. **Control transport: the `adb_client` crate**, gated behind a new
   `android` Cargo feature that is **off by default**. Default builds do not
   pull `adb_client` (it drags in `rustls`/`rsa`/`image`/etc.), preserving the
   ~12 MB single-binary story. This follows the opt-in-dependency precedent
   set for Ollama in ADR-023.

   A spike validated the full screenshot → find-element → tap → verify loop in
   pure Rust against an emulator, both via the adb server *and* via a direct
   TCP connection to `adbd` with the adb server killed (no `adb` binary, no
   server).

2. **Element targeting via the accessibility tree, not vision coordinates.**
   Resolve targets through `uiautomator dump` bounds (`tap_text`/`find_element`)
   rather than model-emitted pixel coordinates. This is the documented fix for
   the targeting bug and is what the agent-facing tool will expose.

3. **Two first-class device providers behind one abstraction: BYO device and
   managed local emulator.** Both are required.
   - **BYO**: the user connects a real phone (USB or wireless debugging) or
     their own already-running emulator. xpressclaw discovers it and controls
     it. Ships nothing but `adb_client`.
   - **Managed emulator**: the user installs the Android SDK; xpressclaw
     orchestrates `sdkmanager` (ensure system image) → `avdmanager` (ensure
     AVD) → `emulator` (boot) → `adb_client` (control). The user pulls Google's
     images themselves, so no redistribution exposure.

   The control layer is **provider-agnostic** — `adb_client` connects to
   whatever `adbd` is present (a phone, or the emulator on `:5555`), so the
   provider abstraction only differs in *provisioning/discovery*, not control.
   `redroid` is recorded as a future third provider (Linux server). We never
   bundle Google's emulator or images.

4. **Agent-facing layer: our own MCP tool** exposing `screenshot`,
   `find_element`/`tap_text`, `tap`, `swipe`, `input_text`, `key_event`,
   superseding `scrcpy-mcp` and removing the implicit image-swap. (MCP remains
   the universal tool interface per ADR-005.)

5. **Human login view.** On desktop the emulator's own window is the login
   surface initially; an embedded stream (scrcpy/noVNC) in the web UI is a
   follow-up. Logins persist via an AVD snapshot.

6. **Surfaced in the Connectors grid as a "device-link" connector** (UX bend).
   For discoverability the Android device appears as a tile in Settings →
   Connectors alongside Telegram/Webhook/etc., even though control is *not* an
   event source/sink. The `AndroidConnector` carries only the adb target
   (serial/tcp); its `validate_config`/`health` probe device reachability via
   `adb_client` (the analog of Telegram's `getMe`). It emits no events and
   rejects `send` — tap/screenshot stay on the MCP/agent path. This bends the
   connector-vs-control separation deliberately, for a familiar setup surface.
   A CLI `xpressclaw android doctor` reports the managed-emulator SDK preflight
   (emulator, system images, AVDs, accel) via `android::sdk::detect()`.

## Consequences

### Positive
- No redistribution/licensing exposure; the only added binary footprint is the
  `adb_client` crate, and only when `--features android` is set.
- Cross-platform (Windows/macOS/Linux) via the local emulator.
- Reliable element targeting (accessibility tree) instead of vision guesses.
- Removes the implicit image-swap and scrcpy version coupling.

### Negative
- Users must install the Android SDK separately (a few GB) — same trade-off
  ADR-023 accepted for Ollama.
- **Google-login integrity risk:** emulators can trip Google's Play Integrity
  checks; AOSP/`redroid` fail certification outright. Fine for non-Google app
  logins; a real risk if the accounts are Google.
- We still shell out to the `emulator` binary to *launch* a device (adb_client
  cannot start an emulator) — but that binary is user-installed, not shipped.

### Risks
- Emulator hardware acceleration (WHPX/KVM/HAXM) must be present on the host.
- The `android` feature must be plumbed consistently across `core`, `cli`,
  `server`, and `tauri` (comprehensive-changes rule).

## Related ADRs
- ADR-003: Container Isolation (redroid would slot in as a provider here)
- ADR-005: MCP Tool System (the agent-facing control tool speaks MCP)
- ADR-023: Ollama-only Local Inference (precedent for opt-in external deps)
