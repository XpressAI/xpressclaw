# ADR-045: Adaptive macOS app icon

## Status

Accepted

## Context

macOS 26 can render app icons in Default, Dark, Clear, and Tinted appearances. XpressClaw's existing ICNS icon is a fixed raster image, so it cannot follow those system appearances.

Tauri does not compile an Icon Composer document as part of this project's bundle process. The release jobs also use macOS 15 runners whose default Xcode is 16.4. Those builds need ready-to-bundle output even when Icon Composer tooling is unavailable or not selected.

## Decision

The two-network XpressClaw mark remains the visual identity. Its geometry is stored as SVG layers inside an Icon Composer document with native appearance settings.

The repository commits the generated `Assets.car`, ICNS fallback, and partial Info.plist. A build script regenerates those files with Apple's `actool` and records SHA-256 hashes of every input and committed output. A platform-specific Tauri configuration adds the catalog and metadata to macOS bundles. CI rejects missing, unexpected, or modified icon files and inspects each release bundle for the required appearance stacks.

Review screenshots and machine-specific verification logs are pull-request evidence. They are not permanent repository artifacts.

## Consequences

macOS releases receive adaptive appearances without changing Windows, Linux, or tray icons. The ICNS fallback preserves the existing minimum macOS version.

The repository gains about 1.8 MB of generated binary data. `actool` does not produce a byte-identical `Assets.car` on repeated runs, so the manifest identifies the reviewed output rather than claiming reproducible compilation. Updating the icon requires Xcode 26 or newer and must update the compiled files and hash manifest in the same commit. If the release workflow later compiles Icon Composer documents reliably on every macOS runner, the committed generated output can be removed.
