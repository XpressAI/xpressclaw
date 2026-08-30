# PowerPoint presentation artifacts

The built-in Codex runner can create a new PowerPoint deck, render every slide
for visual inspection, validate the final OOXML package, and attach the `.pptx`
to its final Task or Project Conversation reply. The attachment is copied into
XpressClaw's durable message storage, so it remains downloadable after the
runner is replaced or the workspace file is removed.

This is an XpressClaw-owned workflow. It is intentionally different from
OpenAI's primary-runtime Presentations skill, which depends on a host-provided
`load_workspace_dependencies` tool and `@oai/artifact-tool` runtime that are not
part of a Codex ACP session. XpressClaw does not emulate that tool, dynamically
install packages during a Task, or advertise the incompatible upstream skill.

## Use it

1. Select the built-in Codex runner for an Agent. The Agent's **Work** page
   shows **PowerPoint artifacts ready** when the compatible image is present.
2. Ask Codex to create a new PowerPoint presentation. The separate
   `xpressclaw-presentations` skill uses the immutable runtime bundled in the
   runner image.
3. Codex keeps an auditable JavaScript builder in the Agent workspace, renders
   every slide through LibreOffice and Poppler, inspects the previews, and
   validates the final `.pptx` before publishing it.
4. Download the presentation card from the final Task or Conversation reply.

The headless workflow currently supports net-new decks. It does not claim
lossless editing of an existing PowerPoint or native Google Slides fidelity.
Existing Office automation can still be configured separately when that is
the appropriate workflow.

## Runtime and publication contract

The built-in image pins PptxGenJS 4.0.1 and exposes this read-only runtime:

```text
/opt/xpressclaw/presentation-runtime
```

The no-argument
`/opt/xpressclaw/presentation-runtime/bin/xpressclaw-presentation-runtime`
command reports absolute Node, module, and helper paths. The runner image is
verified at build time by authoring, rendering, and validating a real one-slide
deck without a model call.

The skill publishes a checked deck with an exact final-response reference:

```text
xpressclaw-file{"path":"/workspace/deck.pptx","title":"Presentation title"}
```

`/workspace` is the ordinary minimal-runner path. The skill uses the absolute
path reported by `pwd` for host-engine runners and adopted nested repositories.

Only a final assistant response is parsed. The source must be a `.pptx`,
`.docx`, `.xlsx`, or `.pdf` inside the Agent's primary writable workspace or
another explicitly configured writable mount. XpressClaw canonicalizes the
path through that mount, rejects traversal and symlink escape, verifies the
declared package format, and caps one message at eight files and 20 MiB total.
The bundled authoring skill remains PPTX-only; the other formats let supported
Office automation and future artifact producers use the same safe durable
delivery boundary. User-authored, escaped, malformed, missing, unsupported, or
out-of-workspace references never cause an arbitrary path read; a valid
reference that cannot be captured becomes a visible unavailable notice.

## Custom Codex images

The simplest compatible custom image extends the built-in Codex runner. An
independent image must provide the same immutable paths and declare both exact
capability labels:

```text
io.xpressclaw.presentations=xpressclaw-pptx-v1
io.xpressclaw.presentations.pptxgenjs=4.0.1
```

Labels are an operator assertion about image contents. Without both labels,
XpressClaw keeps the custom image usable but does not add the presentation
skill root. It also disables the incompatible OpenAI primary-runtime
Presentations and Spreadsheets skills so Codex does not promise a workflow the
ACP host cannot execute.
