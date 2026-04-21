# ADR-023: Pi-Agent as First-Class Harness, WASM Containers via c2w

## Status
Accepted

## Context

Xpressclaw's founding framing, from ADR-001/ADR-002, was that we are a **runtime**, not another agent framework. The runtime runs agents built in other frameworks. In practice, the frame we want to commit to is stronger: Xpressclaw is a **meta-harness** — it runs *agent harnesses*.

A harness is a self-contained program that gives a model a workspace (filesystem, shell, UI surface) plus a loop that drives the model against that workspace. Coding harnesses dominate today because a command shell is a near-optimal interface for both LLMs and programs: codex, opencode, and pi are all variants of the same idea. Future harnesses will specialize — CAD, design, simulation — and Xpressclaw should run them with the same machinery it uses for coding harnesses.

The goal for Xpressclaw itself stays simple: `init && up` to install and go; 24/7 operation on local LLMs to keep budget bounded; vetted extensions via a hub (Apple Store / Debian model) so users can trust what they install. Two pieces of our current stack are now the main obstacles to that goal:

1. **Docker is growth-hampering friction.** Users hit a sign-up wall, paywall nags, and a heavy install before Xpressclaw can do anything. OpenClaw's "just install and run" posture has proven that friction costs users — but OpenClaw pays the reciprocal price that a weak local model or a bad skill can compromise the host. Containers remain the right answer; Docker is the wrong delivery of that answer.
2. **Our default harness — the Claude Agent SDK running in a Docker container — isn't the harness shape the community is converging on.** Pi is popular enough that Jensen Huang wore a lobster (pi's mascot) at GTC. Pi runs the agent inside a tmux session, which makes the agent's actions visible at a glance and lets the user attach, inspect, and intervene. That pattern is strictly better UX than a black-box container exec.

VMs do not solve the friction problem (macOS vmkit caps at 2 concurrent VMs; per-agent VMs are infeasible). The alternative that's already working in the wild is **container2wasm (c2w)**: a toolchain that compiles an OCI image to a WASM module runnable in a sandboxed WASM runtime. Pi uses c2w today. A WASM runtime gives us (a) a snapshot/restore primitive for free, so when a weak model runs `rm -rf` in the wrong place we roll back instead of firefighting, and (b) an embed story like llama-cpp — we ship one binary, no external daemon.

The spike on branch `spike/wanix-agents` (PR #98) explored browser-based Wanix first, then pivoted mid-branch to pi-agent + c2w. The architectural discovery was correct; the execution accumulated 13k lines across a drifting scope, a stale tmux-less processor rewrite, a broken Integration Tests run, and zero git notes. This ADR exists so the next attempt has an anchor to snap back to.

Pi does not, and on principle will not, speak MCP. Xpressclaw's value-adds (shared memory, task board, budget enforcement) are exposed as MCP tools today. Bridging those to a pi agent without MCP is a design constraint, not a blocker — shell is pi's native interface, so we give it shell verbs.

## Decision

1. **Xpressclaw is a meta-harness.** The `Harness` abstraction (ADR-002's "agent backend") is the core extension point. Our built-in harness (Claude Agent SDK + processor loop) is one implementation among peers.

2. **Pi is the first external harness we support as a first-class citizen.** Not wrapped, not shimmed — installed, launched, and inspected with the same lifecycle APIs as our built-in. Tmux-visible execution comes with it and becomes a general Xpressclaw primitive (ADR-013's enhanced chat gains a "watch the session" mode for any harness that exposes one).

3. **container2wasm (c2w) is Xpressclaw's container runtime, hosted by wasmtime, both embedded statically like llama-cpp.** Users install Xpressclaw, not Docker. The runtime lives in `crates/xpressclaw-core/src/runtime/c2w/` and is built by `build.rs` the same way the embedded frontend and local-model weights are.
   - **Why wasmtime:** c2w's README ranks it as the first-class host with full stdio / mapdir / networking coverage (wasmer and wasmedge are explicitly WIP). It's Rust-native (`cargo add wasmtime wasmtime-wasi`), WASIp2 is stable, and it's used in production by Fastly, Shopify, and Microsoft.
   - **Rollback primitive:** wasmtime's epoch interruption plus `Store`-level isolation gives us interrupt-and-discard per agent step. A snapshot is "the guest state *before* we dispatched this tool call"; if the step fails, we drop the `Store` and restart from the prior snapshot. No full-VM snapshot primitive is needed.

4. **Docker support is removed.** No config flag, no fallback. This ADR supersedes ADR-003 in full. Existing Docker-backed agents are migrated to c2w on upgrade; if migration is infeasible for a given user, they stay on the pre-ADR-023 release until they're ready to move.

5. **Harness images are distributed via GitHub Container Registry (GHCR).** Official harnesses live under `ghcr.io/xpressai/harnesses/<name>:<version>` as OCI images (the same format c2w takes as input). `xpressclaw init` pulls the default set; `xpressclaw harness add <ref>` pulls additional ones. GitHub auth piggybacks on the user's `gh` CLI credentials when images are private. This is the interim story; the hub (out of scope here) will add signing and vetting on top of GHCR, not replace it.

6. **Xpressclaw's sidecar is the single LLM endpoint for every harness.** Harnesses are configured with one `OPENAI_API_BASE` (or equivalent) pointing at the Xpressclaw sidecar — nothing else. The sidecar owns all provider API keys and all routing logic:
   - Per-request: picks the backend based on the agent's configured model + current budget state.
   - Transparent downgrade: when an agent hits its budget cap and is configured with `on_exceeded: degrade`, the sidecar swaps the outbound call to the local llama-cpp model for the remainder of the window. The harness sees a different response but no error, no reconfiguration, no restart.
   - Streaming, tool calls, and cancellation are proxied end-to-end.
   - Harnesses never hold API keys and never pick providers. This is non-negotiable — it's what makes budget enforcement real.

7. **The MCP bridge for non-MCP harnesses is a small `xclaw` CLI mounted into the harness workspace.** Pi's agent invokes `xclaw memory add "..."`, `xclaw task update ...`, `xclaw budget`. The CLI speaks to the Xpressclaw server over a Unix socket mounted into the container. Structured output uses exit codes + JSON on stdout. This respects pi's "shell-verbs-not-MCP" stance and generalizes to any future shell-native harness.

8. **V1 ships two harnesses and stops.** `PiHarness` (new) and our built-in Claude-SDK-style harness retrofitted onto the `Harness` trait. Codex, opencode, and others come only after V1 ships and sticks. Support a small number well before expanding.

9. **The hub (vetted harness + skill distribution) is explicitly out of scope for this ADR.** It is the downstream consumer of this work. Naming it here so future ADRs can reference a clear boundary.

### Harness abstraction shape

```rust
trait Harness {
    fn id(&self) -> &str;
    async fn start(&self, agent_id: &AgentId, spec: &HarnessSpec) -> Result<HarnessHandle>;
    async fn send_user_message(&self, handle: &HarnessHandle, msg: &str) -> Result<()>;
    async fn attach_tmux(&self, handle: &HarnessHandle) -> Result<TmuxSocket>;
    async fn snapshot(&self, handle: &HarnessHandle) -> Result<SnapshotId>;
    async fn restore(&self, handle: &HarnessHandle, snap: &SnapshotId) -> Result<()>;
    async fn stop(&self, handle: &HarnessHandle) -> Result<()>;
}
```

V1 ships two implementations: `PiHarness` (new) and `ClaudeAgentSdkHarness` (retrofit our existing built-in onto this trait). Further harnesses (`CodexHarness`, `OpencodeHarness`, specialized non-coding harnesses) are follow-up work. Each backend picks its own tool bridge — MCP-native harnesses skip the `xclaw` CLI and talk MCP directly.

### LLM routing through the sidecar

```
   ┌─────────────┐          ┌──────────────────────────────┐
   │  Harness    │          │  Xpressclaw Sidecar          │
   │  (pi, sdk)  │──HTTP───▶│  /v1/chat/completions        │
   │             │          │                              │
   │ OPENAI_API_ │          │  budget check ─┐             │
   │  BASE=sidecar          │                ▼             │
   └─────────────┘          │  ┌──────────────────────┐   │
                            │  │  router              │   │
                            │  │  - under budget →    │   │
                            │  │    configured prov.  │──┼──▶ Anthropic / OpenAI / …
                            │  │  - over + degrade →  │   │
                            │  │    llama-cpp local   │──┼──▶ embedded llama-cpp
                            │  │  - over + pause →    │   │
                            │  │    429 + hold queue  │   │
                            │  └──────────────────────┘   │
                            └──────────────────────────────┘
```

The sidecar exposes a single OpenAI-compatible endpoint (maximum harness compatibility). Every request carries an agent-scoped token so the router knows whose budget and config to apply. Downgrade is idempotent — a mid-conversation swap to local lands on whatever model was last successful, no harness-visible state change. Providers' native SDKs (Anthropic, Gemini) are reachable by harnesses that insist on them, via per-provider OpenAI-compatible shims the sidecar emits.

### MCP bridge surface for `xclaw` CLI

Initial verbs, picked to cover the MCP tool categories Xpressclaw ships today:

| Verb | Maps to |
|------|---------|
| `xclaw memory add/search/list/delete` | `memory` MCP server |
| `xclaw task create/update/status/list` | `tasks` MCP server |
| `xclaw budget` | `budget` MCP server |
| `xclaw log <level> <message>` | activity log |
| `xclaw ask <question>` | pauses agent, surfaces to user via conversation |

Streaming / long-lived tool calls that don't fit shell semantics are deferred; any harness needing them gets MCP-native via its own bridge.

## Consequences

### Positive
- **Install friction drops to near zero.** No Docker install, no Docker Desktop license prompt. `xpressclaw init && up` is again literally true.
- **Rollback-on-failure is a default, not a feature.** wasmtime epoch interruption + per-agent `Store` isolation means every step boundary is a free checkpoint.
- **Tmux-visible execution is the default UX** — observability stops being an afterthought.
- **Pi's ecosystem gains Xpressclaw's value-adds** (persistent memory, cross-agent tasks, budget) without pi itself changing.
- **Budget enforcement becomes real.** Because every harness is forced through our LLM sidecar, the transparent downgrade to local is an architectural property — not a harness has to cooperate. "Run 24/7 on local when the budget is spent" stops being a promise and becomes a guarantee.
- **One runtime, not two.** Dropping Docker halves the isolation code we have to maintain and removes the worst of the platform-specific quirks (Docker Desktop's macOS filesystem performance, WSL2 on Windows, GPU passthrough matrices).

### Negative
- **c2w is young.** Image compatibility, syscall coverage, startup latency, and filesystem durability are all in flux. We will hit bugs and need to carry patches or upstream fixes. No Docker fallback means every one of these bugs blocks users until we fix it.
- **Existing Docker users can't upgrade seamlessly.** Removing Docker support entirely means agents with Docker-specific volume mounts or images need migration. Mitigation: honor pre-ADR-023 installs as frozen — they continue to work on older binaries — and provide a one-way migration tool for agents whose config is Docker-independent.
- **Shell-verb bridge is less expressive than MCP** for tools that stream, return structured errors, or hold bidirectional state. We accept this for the harnesses that can't do MCP and keep MCP for the ones that can.
- **Embedding c2w + wasmtime grows the binary.** llama-cpp already makes us large. We'll need to measure and possibly ship a "core" binary without the local-model weights for users who bring their own LLM.
- **GHCR rate limits apply to anonymous pulls.** Users who don't authenticate with `gh` may hit pull caps if they thrash. Mitigation: prompt for `gh auth` during `xpressclaw init` when the user has `gh` installed; cache pulled images locally aggressively.

### Risks
- **c2w might not cover real pi workloads out of the box.** Pi expects a functioning Linux userland. Mitigation: the MVP exit criteria below prove one end-to-end flow before we consider the direction validated.
- **Snapshotting a live tmux session is an edge case.** A pty with a half-written escape sequence is not trivially serializable. Mitigation: snapshot at step boundaries (after each tool response), not mid-stream.
- **Sidecar becomes a single point of failure for LLM traffic.** If the sidecar crashes, every agent stalls. Mitigation: supervise with automatic restart; health-check from each harness's c2w runtime on a separate timer.
- **Scope drift.** The last spike failed not on architecture but on scope. Mitigation: this ADR defines V1 as two harnesses and the MVP below. Everything else is a follow-up ADR.

## Open Questions

1. **Pi's license and upstream posture.** Can we patch pi? Contribute? The bridge design changes if we must treat pi as an opaque binary vs. a codebase we can modify. Needs research before we start the PiHarness implementation.
2. **Streaming / bidirectional tools over the `xclaw` shell bridge.** Simple request/response verbs map cleanly; long-lived subscriptions (e.g. "watch for task updates") do not. Current plan is to defer these — harnesses that need them go MCP-native. If pi usage surfaces a must-have streaming verb, we revisit.
3. **c2w networking egress policy.** c2w supports container networking via a WASI-networking shim. We need to decide per-harness defaults: egress allowlist, block-by-default, or open. MVP assumption is "outbound TCP to the sidecar socket only." Real harnesses will push on this.

## MVP / Exit Criteria

This ADR is validated when a fresh user can:

1. Install Xpressclaw (one binary, no Docker, no other dependencies).
2. Run `xpressclaw init` and `xpressclaw up`; the pi harness image pulls from `ghcr.io/xpressai/harnesses/pi` on first use.
3. Create a pi-harness agent configured for a cloud provider (Anthropic or OpenAI), with a daily budget cap of e.g. $0.50.
4. Send one task ("write hello.py and run it"), watch it execute in the agent's tmux session.
5. Observe the agent calling `xclaw memory add` to persist a note, and see that note appear in the global memory view.
6. Exhaust the budget mid-task; observe the sidecar transparently switch the outbound LLM calls from the cloud provider to the embedded llama-cpp model, with no harness error and no user intervention.
7. Trigger a deliberate failure (agent runs `rm -rf /`) and observe automatic rollback to the pre-step snapshot without Xpressclaw itself being affected.

Hitting all seven is the signal to write the follow-up ADRs (hub, additional harnesses, cross-harness collaboration).

## Related ADRs

- **Supersedes (in full):** ADR-003 (Container Isolation) — Docker is removed; c2w on wasmtime is the sole runtime.
- **Extends:** ADR-002 (Agent Backend Abstraction) — formalizes the `Harness` trait and makes the built-in backend one harness among peers.
- **Touches:** ADR-005 (MCP Tool System) — adds a shell-verb bridge (`xclaw` CLI) alongside native MCP for harnesses that can't speak MCP.
- **Touches:** ADR-010 (Budget Controls) — transparent downgrade becomes an architectural property of the sidecar, not a per-harness feature.
- **Touches:** ADR-013 (Enhanced Agent Chat) — tmux-attach becomes a UI primitive.
- **Touches:** ADR-017 (Agent Apps) — harness lifecycle interacts with the app reconciler.
- **Does not supersede:** ADR-011 (Default Local Model) — llama-cpp embed is unchanged (and its role is promoted: it's now the budget-fallback target for every harness).
