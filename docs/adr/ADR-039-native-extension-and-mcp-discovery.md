# ADR-039: Native Extension and MCP Discovery

## Status

Proposed

## Context

XpressClaw keeps skills, plugins, hooks, and other extensions in each ACP
harness's native configuration. The Harness page explains that boundary, but
does not help a user find the selected harness's current extension
documentation. The shared MCP catalog also begins as an empty manual form
without a path to the protocol's official registry.

This makes the product's ownership model correct but hard to discover. A new
user must already know where external servers and harness extensions are
documented, and the difference between a remote MCP endpoint and a stdio
package inside a runner image is easy to miss.

## Decision

XpressClaw will add a documentation-only discovery layer to the existing MCP
and native-configuration surfaces.

- The shared MCP settings page and each Agent's MCP section link to the
  official MCP Registry.
- The native-configuration section links to the selected harness's official
  skills guide when one is available.
- Other catalogued harnesses fall back to their official installation or
  documentation page.
- A custom harness does not show a documentation action because XpressClaw
  cannot identify an authoritative source for it.
- External documentation opens through the existing system-browser path.
- The MCP empty state distinguishes remote HTTP/SSE endpoints from stdio
  packages, which must already exist at an absolute path inside each runner
  image.

The implementation remains frontend-only. It consumes the existing harness
catalog and MCP configuration APIs without adding a registry proxy, installer,
or new persistence model.

## Architecture boundary

Native harnesses remain authoritative for their own skills, plugins, hooks,
custom agents, and configuration as established by ADR-025. XpressClaw helps a
user find the relevant official source but does not translate those extensions
into a proprietary format.

MCP remains the common tool protocol established by ADR-005. XpressClaw links
to the official registry but does not mirror, rank, parse, or proxy it.

## Non-goals

- Installing third-party packages or extensions.
- Mirroring or embedding an external marketplace.
- Automatically attaching discovered MCP servers to an Agent.
- Parsing or proxying the MCP Registry API.
- Creating an XpressClaw-specific skill or extension format.

## Consequences

### Positive

- Empty MCP catalogs have an obvious discovery path.
- Harness documentation follows the selected catalog entry while keeping the
  native product in control.
- Users receive clearer guidance for remote and stdio MCP configurations.
- The change does not expand the backend security or package-execution
  boundary.

### Negative

- Official documentation URLs can move and must be revalidated when mappings
  change.
- Opening external documentation takes the user out of the application.
- Harnesses without a dedicated skills guide receive a broader documentation
  fallback.

## Acceptance criteria

1. Global and per-Agent MCP settings link to the official MCP Registry.
2. The selected catalogued harness resolves to an official skills guide or
   official documentation fallback.
3. A custom harness shows no misleading documentation link.
4. MCP empty-state copy explains the stdio runner-image requirement.
5. Discovery actions remain usable at desktop and narrow viewport widths.
6. Existing MCP add, edit, delete, attach, and verification behavior remains
   unchanged.

## Documentation sources

Revalidated from their official owners on 2026-08-16:

- MCP Registry: https://registry.modelcontextprotocol.io/
- OpenAI skills: https://learn.chatgpt.com/docs/build-skills
- Claude Code: https://code.claude.com/docs/en/skills
- GitHub Copilot CLI: https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-skills
- OpenCode: https://opencode.ai/docs/skills/
- Qwen Code: https://qwenlm.github.io/qwen-code-docs/en/users/features/skills/
- Cline: https://docs.cline.bot/customization/skills
