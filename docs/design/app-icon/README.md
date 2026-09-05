# XpressClaw macOS app icon

XpressClaw uses an Icon Composer document for adaptive macOS app appearances while preserving the existing two-network mark.

The editable sources are under `crates/xpressclaw-tauri/icons/macos/`. The committed `compiled/` directory contains the asset catalog used by release builds and an ICNS fallback for older macOS versions. ADR-045 records why the generated files remain in version control.

## Rebuild

Run this from the repository root on macOS with Xcode 26 or newer:

```sh
python3 scripts/build-app-icon.py
```

Set `DEVELOPER_DIR` to use a non-default Xcode installation. The script regenerates the SVG geometry, compiles the Icon Composer document with `actool`, updates the source hash manifest, and verifies its result. It does not alter the appearance settings stored in `XpressClaw.icon/icon.json`.

To check the committed files without Xcode:

```sh
python3 scripts/build-app-icon.py --check
```

The macOS Tauri configuration runs the same check before bundling. CI also verifies that the resulting application contains the committed catalog, the fallback icon, the expected Info.plist keys, and all required appearance stacks.

Verify a built bundle with:

```sh
python3 scripts/verify-app-icon.py --app target/aarch64-apple-darwin/release/bundle/macos/xpressclaw.app
```

Use `--skip-signature` only for a local unsigned build. Release verification requires a valid signature.
