# ADR-029: Project Memory over MCP

## Status

Accepted

## Context

XpressClaw can keep one ACP conversation alive across many tasks, but that
conversation is not a durable, structured project knowledge base. Harnesses
also differ: some have native instruction files or memory features, while
others have no project-memory mechanism at all. Depending on a harness-specific
feature would make knowledge disappear when a project changes agents.

An earlier XpressClaw memory implementation injected recalled text before every
model turn and extracted new memory afterward. Those invisible hooks confused
agents, mixed unrelated scopes, and made it difficult to understand why context
had appeared. The old tables remain for compatibility, but they are not the
right contract for multiplexed native sessions.

## Decision

XpressClaw owns an explicit memory store for each logical project session and
exposes it through the built-in project-scoped MCP server.

Each memory is an atomic Zettelkasten-style note with:

- a title, summary, Markdown body, type, lifecycle state, and tags;
- optional task and work-attempt provenance;
- an author category and pin state; and
- directed, typed links to other notes in the same project.

Typed links represent asserted knowledge relationships. Vector similarity
never silently creates a link.

Retrieval is hybrid. Lexical matching uses NFKC normalization and full Unicode
case folding, including Japanese compatibility-width forms. A local
character-trigram vector provides offline similarity candidates through
sqlite-vec. The vector table uses `project_id` as a vec0 partition key so
nearest-neighbour lookup cannot return another project's notes. The embedding
model is reported honestly as surface-similarity retrieval; a multilingual
semantic embedding provider can replace it later without changing the MCP
contract.

The MCP server exposes read, search, create, update, link, and archive tools. It
also advertises a high-priority project briefing, structured index, and note
resource template. Initialization instructions tell agents that memory exists
and include compact counts and topics when the control plane is available.
Because MCP clients vary in how they surface resources, the same information is
available through model-callable tools and ordinary text tool results.

Memory writes are explicit in this phase. XpressClaw does not run hidden
pre-turn recall or post-turn extraction hooks.

## Consequences

Project knowledge survives task and harness changes without being coupled to a
single long conversation. Users and agents can inspect provenance and correct,
supersede, link, or archive notes. Unicode-aware retrieval and database-level
partitioning are testable invariants.

Agents must exercise judgment about what is durable enough to store, and the
initial trigram embedding captures textual rather than deep semantic
similarity. A later memory-management phase may add a browser/editor,
suggested-link review, semantic embedding providers, deduplication, and
automatic upkeep. Those features must preserve explicit provenance and avoid
reintroducing invisible prompt hooks.
