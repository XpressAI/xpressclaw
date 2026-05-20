# Android POC — Gotchas Encountered

Notes from the first end-to-end attempt at driving an Android emulator via
`scrcpy-mcp`. What broke, why, and what fixed it. Companion to
`docs/android-poc.md` (the happy-path walkthrough) and ADR-024.

## 1. Workspace wouldn't compile

`cargo check -p xpressclaw-core` failed with an `async_trait` lifetime
mismatch between `procedure_runner.rs` and
`external/ready-agent-cog/src/tools/traits.rs`.

**Cause.** The submodule was checked out at commit `50fc17d`
("Include code when retrying generation") while the parent repo's
reference was `b4e790d`. The newer submodule commit changed the
`ToolsModule::execute` trait signature in a way `procedure_runner.rs`
doesn't satisfy.

**Fix.**
```bash
git submodule update --force external/ready-agent-cog
```

Resets the submodule to the ref the parent expects. Verify with
`git diff external/ready-agent-cog` returning empty.

## 2. No Android emulator infrastructure on the host

`~/android-sdk` had `platform-tools` and `cmdline-tools` only — no
`emulator/` package, no system images, no AVDs. So `adb` worked but
nothing to talk to.

**Fix.**
```bash
SDK=~/android-sdk
$SDK/cmdline-tools/latest/bin/sdkmanager "emulator" "system-images;android-35;google_apis;x86_64"
yes | $SDK/cmdline-tools/latest/bin/sdkmanager --licenses
$SDK/cmdline-tools/latest/bin/avdmanager create avd -n pixel_test \
  -k "system-images;android-35;google_apis;x86_64" -d "pixel_6"
```

Picked Android 35 + Google APIs (not Play Store — avoids signing
complications for the POC).

## 3. `/dev/kvm` ACL ≠ `kvm` group membership

Initial check: `groups` showed no `kvm` membership, so I almost ran
`sudo usermod -aG kvm $USER` and asked the user to re-login.

**Don't.** `getfacl /dev/kvm` showed `user:fahreza:rw-` — an ACL grants
this user direct access without group membership. Always check
`getfacl /dev/kvm` AND `python3 -c "open('/dev/kvm','rb')"` before
assuming a group fix is needed.

## 4. `/` boot disk at 99% (1.4 GB free)

Emulator package + system image + AVD is ~5 GB. Would have filled the
boot disk and likely caused instability.

**Fix.** Free space first. Worst offenders for this user:

| Path | Reclaimed |
|---|---|
| `~/.cache` (HF models, Playwright/Puppeteer Chromiums, uv) | 16 GB |
| `~/.npm` | 11 GB |
| Github `node_modules` + `.next` (excluding active projects) | ~3 GB |
| Github `.venv` dirs across 27 repos | ~6 GB |

Total: ~36 GB. `df -h /` jumped 99% → 63%.

**Lesson.** Cleanup before installing big SDKs. Don't be optimistic
that ~1 GB free is enough.

## 5. `emulator` not on PATH

`~/android-sdk/emulator/` was a separate dir from `platform-tools/`.
The user's `.bashrc` only had platform-tools + cmdline-tools on PATH.

**Fix.** Append to the existing `export PATH=...` line in `.bashrc`:
```bash
export PATH="$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH"
```

## 6. Headless emulator has two boot phases

`adb get-state` returns `device` ~20 seconds after `emulator` launches,
but Android userspace isn't ready yet — apps won't launch, screenshots
return blank-ish images.

**Fix.** Wait for both signals before doing anything real:
```bash
adb get-state                              # waits for adb visibility
adb shell getprop sys.boot_completed       # waits for userspace boot (returns "1")
```

For headless boot of a fresh Pixel 6 AVD, full boot took ~70 seconds
end-to-end.

## 7. First screenshots returned ~15 KB lockscreen-ish images

After boot, `adb exec-out screencap -p > x.png` produced ~15 KB PNGs.
Settings was supposedly launched but the screenshot didn't show it.

**Cause.** Screen was off / device locked. The intent fires but the
foreground activity isn't visible.

**Fix.**
```bash
adb shell input keyevent KEYCODE_WAKEUP
adb shell input keyevent KEYCODE_MENU       # dismiss lockscreen on fresh AVD
adb shell am start -n com.android.settings/.Settings
```

After that, the screenshot jumped to ~149 KB — real content.

**Verify before trusting a screenshot.** Cross-check:
```bash
adb shell dumpsys activity activities | grep -E "topResumedActivity"
```

The `topResumedActivity` should match what you launched.

## 8. `scrcpy-mcp start_session` failed with "Failed to connect to scrcpy server on port 27183"

Most `scrcpy-mcp` tools (screenshot, tap, swipe, app_start, ui_dump,
shell_exec, …) work via an ADB fallback path even without `scrcpy`.
But `start_session` requires actually starting `scrcpy` on the host,
which pushes `scrcpy-server` to the device and opens a TCP tunnel.

**Cause.** Ubuntu's `apt install scrcpy` ships **scrcpy 1.25** (years
old). `scrcpy-mcp` is written against modern scrcpy (v2.x / v3.x / v4.x)
and the server-protocol contract changed.

**Fix.** Install modern scrcpy from the GitHub release. Snap (v3.3.4)
also works but the user preferred a portable binary:

```bash
mkdir -p tools/scrcpy && cd tools/scrcpy
curl -fsSL -o scrcpy.tar.gz \
  https://github.com/Genymobile/scrcpy/releases/download/v4.0/scrcpy-linux-x86_64-v4.0.tar.gz
tar xzf scrcpy.tar.gz --strip-components=1 && rm scrcpy.tar.gz
ln -sf "$PWD/scrcpy" ~/.local/bin/scrcpy   # shadows apt scrcpy on PATH
scrcpy --version    # should report 4.0
```

The GitHub Linux tarball is fully portable — includes `scrcpy`,
`scrcpy-server`, and a bundled `adb`. No system install required.

Verify the apt version is shadowed:
```bash
which -a scrcpy
# /home/<you>/.local/bin/scrcpy
# /usr/bin/scrcpy
```

`scrcpy-mcp` finds `scrcpy-server` automatically when both live next to
the `scrcpy` binary. If it doesn't (e.g. you installed only the binary
elsewhere), set `SCRCPY_SERVER_PATH=/path/to/scrcpy-server` in the env
that launches `scrcpy-mcp`.

### 8a. Even after installing v4.0, `start_session` still fails with a version mismatch

After installing the v4.0 portable binary, `start_session` still
errored. Inspector log line revealed the actual problem:

```
[scrcpy-server] java.lang.IllegalArgumentException:
  The server version (1.25) does not match the client (4.0)
```

The **client** (host binary) is v4.0, but a **v1.25 `scrcpy-server`
JAR** is being pushed to the device.

**Cause.** Even after `apt install scrcpy` installed v1.25, removing or
shadowing the binary doesn't remove the server JAR. The apt package
leaves `/usr/share/scrcpy/scrcpy-server` (41 KB, dated 2023) on disk.
Modern scrcpy's server-path lookup order is roughly:

1. `$SCRCPY_SERVER_PATH` (if set)
2. Same dir as the executable (resolved via argv[0])
3. `/usr/local/share/scrcpy/scrcpy-server`
4. `/usr/share/scrcpy/scrcpy-server` ← finds the apt v1.25 JAR here

Step 2 may not trigger as expected if `scrcpy` is invoked via a symlink
in `~/.local/bin` — the binary itself resolves to the project dir, but
the lookup behavior depends on the scrcpy build.

**Fix — pick one.**

Quick / no sudo (env var, scoped to that shell):
```bash
export SCRCPY_SERVER_PATH=/mnt/extra/Github/xpressclaw/tools/scrcpy/scrcpy-server
npx -y @modelcontextprotocol/inspector npx -y scrcpy-mcp
```
Add the `export` to `~/.bashrc` to persist.

Permanent / clean (removes the v1.25 cruft entirely):
```bash
sudo apt remove scrcpy
```
After this, `/usr/share/scrcpy/scrcpy-server` is gone, your v4.0 binary
falls through to step 2 of the lookup order and finds its own v4.0 JAR,
and `SCRCPY_SERVER_PATH` becomes unnecessary.

**Verify the fix.** A successful `start_session` response looks like:
```json
{
  "status": "connected",
  "serial": "emulator-5554",
  "screenSize": { "width": 2147483648, "height": 460 },
  "message": "scrcpy session active. Input and screenshots will use the fast path."
}
```
The `width: 2147483648` (2³¹) is a cosmetic integer overflow in
`scrcpy-mcp`'s resolution parsing — input and screenshots still use
correct coordinates internally.

## 9. `scrcpy-mcp` fallback path is unobvious

If `scrcpy` is missing or wrong-version, only a handful of tools fail:

| Strictly needs scrcpy | Falls back to ADB |
|---|---|
| `start_session` / `stop_session` | `screenshot` (~500 ms instead of ~33 ms) |
| `expand_notifications`, `expand_settings`, `collapse_panels` | `tap`, `swipe`, `long_press`, `drag_drop` |
| `rotate_device` | `input_text`, `key_event`, `scroll` |
| `screen_record_*` | `app_start`, `app_stop`, `app_list`, `app_current` |
| `start_video_stream` / `stop_video_stream` | `ui_dump`, `ui_find_element`, `shell_exec` |
| `clipboard_*` (Android 10+ via scrcpy only) | `file_push`, `file_pull`, `file_list`, `device_list`, `device_info` |

**Lesson.** Don't assume "scrcpy broken = demo broken." Skip
`start_session`, every interactive tool still works.

## 10. MCP screenshot tool returns image content, not a file path

`scrcpy-mcp`'s `screenshot` tool returns:

```json
{
  "result": {
    "content": [{
      "type": "image",
      "mimeType": "image/png",
      "data": "<base64>"
    }]
  }
}
```

This is the standard MCP `image` content block. Claude Agent SDK
consumes it natively as a vision input. To inspect from a raw JSON-RPC
test:

```python
import json, base64
r = json.loads(line_with_id_2)
png_bytes = base64.b64decode(r["result"]["content"][0]["data"])
open("/tmp/out.png", "wb").write(png_bytes)
```

## Quick triage flow when something breaks

```bash
adb devices                                 # device visible?
adb shell getprop sys.boot_completed        # fully booted?
scrcpy --version                            # is it 2.x+?
which scrcpy                                # is the right one on PATH?
ls -la ~/.local/bin/scrcpy                  # symlink intact?
ls /usr/share/scrcpy/scrcpy-server 2>&1     # apt leftover JAR? (see 8a)
echo $SCRCPY_SERVER_PATH                    # set if apt cruft still present
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | npx -y scrcpy-mcp 2>&1 | head -5
```

If those pass, the rest is the agent / model / prompt.
