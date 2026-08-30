---
name: xpressclaw-presentations
description: Create new PowerPoint PPTX decks with XpressClaw's pinned headless presentation runtime, render every slide for visual QA, validate the OOXML package, and publish the finished deck as a Task or Conversation attachment. This is an XpressClaw workflow, not OpenAI's artifact-tool Presentations skill.
---

# XpressClaw presentations

Use this skill for a new local PowerPoint `.pptx` deck in an XpressClaw Codex ACP runner. It is intentionally separate from OpenAI's primary-runtime Presentations skill: this workflow uses pinned PptxGenJS, LibreOffice, and Poppler, and does not provide or emulate `@oai/artifact-tool` or `load_workspace_dependencies`.

Do not install packages or search for alternate runtime paths. Do not use this workflow to edit an existing deck or claim native Google Slides fidelity. For an existing PowerPoint, use the separately configured Office automation tool when available or explain that this headless workflow currently supports net-new decks.

## Runtime contract

Before authoring, run this exact no-argument command once:

```bash
/opt/xpressclaw/presentation-runtime/bin/xpressclaw-presentation-runtime
```

It returns JSON containing absolute `node`, `node_modules`, `bin`, and `module` paths plus the runtime capability and PptxGenJS version. Treat a failure or a capability other than `xpressclaw-pptx-v1` as a blocker. Never modify anything below `/opt/xpressclaw/presentation-runtime`.

Run `pwd` and treat that absolute directory as the active writable repository/workspace; do not assume it is always `/workspace` because host-engine and adopted-repository sessions may use another absolute path. Create one auditable `.mjs` builder there. Import the runtime only through the returned absolute module path:

```js
import PptxGenJS from "/opt/xpressclaw/presentation-runtime/pptxgenjs.mjs";
```

Use the builder as the source of truth and rerun it after every correction. Set deck metadata, language, a 16:9 layout unless the user asks otherwise, and an intentional theme with explicit fonts and colors. Prefer a small number of strong layouts over dense slides. Keep titles at least 28 pt, body copy at least 16 pt, align to a consistent grid, and avoid placing content near slide edges. Use diagrams, charts, shapes, and properly licensed workspace images when they communicate better than prose. Do not fabricate citations or data.

For raster input, use only PNG or JPEG; use SVG for vectors. Do not pass ICNS, JXL, HEIF, or another unsupported image container to the authoring runtime.

## Render and visual QA

After every material build, render the deck:

```bash
/opt/xpressclaw/presentation-runtime/bin/xpressclaw-render-slides "$PWD/deck.pptx" "$PWD/deck-rendered"
```

Inspect every emitted `slide-N.png` with the available image-viewing tool. Check clipping, overlap, tiny text, weak contrast, inconsistent spacing, awkward line breaks, chart labels, and unintended blank slides. Fix the builder and repeat the render until every slide passes. A successful command is not visual QA.

Then validate the final OOXML package and a clean render:

```bash
/opt/xpressclaw/presentation-runtime/bin/xpressclaw-validate-presentation "$PWD/deck.pptx"
```

## Publish

Keep the builder and rendered previews in the workspace unless the user asks for them. Publish the final deck by placing this exact reference in the final assistant response, with an absolute path inside the writable workspace:

```text
xpressclaw-file{"path":"/absolute/path/reported/by/pwd/deck.pptx","title":"Presentation title"}
```

Replace the example path with the real absolute path reported by `pwd`; do not emit the placeholder literally. XpressClaw copies the file into durable message attachment storage. Do not use `file://`, `sandbox:`, a Markdown link to an absolute path, or a path outside the workspace. Include ordinary concise prose before the reference; do not claim delivery unless the build, visual inspection, validation, and publication reference all succeeded.
