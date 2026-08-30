# ADR-042: Codex ACP Presentation Artifacts

## Status

Accepted

## Context

XpressClaw can mount a user's `~/.codex` tree into the built-in Codex runner.
That makes OpenAI's primary-runtime Presentations and Spreadsheets skill text
visible, but it does not supply the host runtime those skills require. The
skills mandate an exact no-argument `load_workspace_dependencies` tool and an
absolute `@oai/artifact-tool` module path. Codex ACP therefore sees a skill it
cannot execute and can return a misleading promise or a missing-tool failure.

The official [App Server dynamic-tool contract](https://developers.openai.com/codex/app-server#dynamic-tool-calls-experimental)
allows a host to provide tool specifications when starting a thread and to
answer subsequent dynamic tool calls. The current Codex ACP adapter does not
expose that host contract to XpressClaw. As of 2026-08-30, the exact
`@oai/artifact-tool` package was not available from the public npm registry,
and no public redistribution terms or supported standalone installation were
found in the OpenAI documentation or the installed primary-runtime plugin.
The observed runtime is supplied by a desktop host cache. Packaging an
invented path or an unlicensed desktop-host payload would not be a supported
implementation.

By contrast, [PptxGenJS 4.0.1 is a public npm release](https://www.npmjs.com/package/pptxgenjs)
and its [upstream package metadata declares the MIT license](https://github.com/gitbrent/PptxGenJS/blob/v4.0.1/package.json),
so that exact fallback dependency can be pinned and redistributed in the
built-in runner.

## Decision

- XpressClaw does not emulate `load_workspace_dependencies` and does not claim
  compatibility with OpenAI's primary-runtime artifact skills.
- Every Codex ACP process explicitly disables the incompatible
  `presentations:Presentations` and `spreadsheets:Spreadsheets` skill entries.
  Developer guidance also forbids dynamically installing or fabricating a
  replacement runtime.
- The built-in Codex image instead packages a separately named,
  XpressClaw-owned `xpressclaw-presentations` skill and an immutable runtime at
  `/opt/xpressclaw/presentation-runtime`. It pins PptxGenJS 4.0.1, LibreOffice,
  Poppler, and metrically compatible Liberation/DejaVu fonts. The image build
  creates, renders, and validates a real deck without a model call.
- The image advertises exact capability and runtime-version labels.
  XpressClaw adds the runtime as an ACP `additionalDirectories` skill root only
  when both labels match. That root participates in the retained-session
  signature, so a capability change replaces/reloads the session before the
  next prompt. Missing or custom runtimes remain usable but do not advertise
  the workflow, and readiness reports the reason.
- The fallback supports net-new `.pptx` decks. Its skill requires an auditable
  workspace builder, a render-and-inspect pass for every slide, and final OOXML
  plus clean-render validation. It is not an alternative implementation of
  the upstream skill and does not promise lossless existing-deck editing.
- A successful final response may contain an exact
  `xpressclaw-file` reference. XpressClaw parses only final assistant output,
  authorizes the canonical source through the Agent's approved writable mount
  roots, bounds and validates PPTX, DOCX, XLSX, or PDF content, then writes it
  into the same durable transaction as the Task or Conversation message.
  Browsers receive only an authenticated attachment endpoint with forced
  download and `nosniff`; the runner path is never exposed.

## Consequences

Codex users get a reproducible create/render/inspect/validate/download workflow
today without violating the upstream skill's contract. A clean or restarted
runner produces durable presentation attachments, while incompatible images
fail honestly and visibly. The built-in Codex image becomes larger because it
contains a headless Office renderer and fonts.

If OpenAI later publishes a redistributable standalone artifact runtime and a
Codex ACP dynamic-tool bridge, this decision can be revisited. Capability
labels and the separately named skill keep that future integration from being
confused with the present fallback.
