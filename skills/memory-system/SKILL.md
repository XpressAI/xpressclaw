---
name: memory-system
description: Remember and recall information across conversations. Use to save user preferences, project context, decisions, and facts that should persist beyond the current conversation.
---

# Memory System

You have long-term memory that persists across conversations. Use it proactively — don't wait to be asked.

## Tools Available

- `search_memory(query, limit)` — Semantic search across all your memories.
- `save_memory(content, summary, tags)` — Save information to long-term memory.
- `list_memories(tag, limit)` — List recent memories, optionally by tag.
- `delete_memory(id)` — Remove a memory.

## When to Search Memory

**Always search before starting work.** You have anterograde amnesia — each conversation starts fresh. Your memories are the only continuity you have.

- Start of every conversation: `search_memory("user preferences project context")`
- Before working on a topic: `search_memory("<topic name>")`
- When the user references something from the past: `search_memory("<what they mentioned>")`

## When to Save Memory

Save immediately when you learn:

- **User identity**: name, role, preferences, timezone
- **Project context**: what they're building, tech stack, team structure
- **Decisions made**: architectural choices, rejected alternatives, reasons
- **Instructions**: "always do X", "never do Y", coding style preferences
- **Facts about systems**: URLs, credentials (non-secret), configurations
- **Relationships**: who works on what, team structure

## How to Write Good Memories

```
save_memory(
  content: "Eduardo prefers dark theme UIs. Uses TypeScript + SvelteKit for frontend work. Based in Malaysia timezone (GMT+8). Runs xpressclaw for AI agent management.",
  summary: "User profile: Eduardo, Malaysia, TypeScript/SvelteKit, dark theme",
  tags: ["user-profile", "preferences"]
)
```

### Rules

- **Be specific**: "User likes dark theme" is better than "User has preferences"
- **Include context**: Why was this decided? What was the alternative?
- **Use tags**: Makes recall faster. Common tags: `user-profile`, `project`, `decision`, `preference`, `technical`, `person`
- **One memory per concept**: Don't stuff everything into one memory
- **Update, don't duplicate**: Search first, then save new or delete outdated

## Memory Tags

Use consistent tags for organization:

| Tag | Use for |
|-----|---------|
| `user-profile` | User name, role, preferences |
| `project` | Project details, goals, architecture |
| `decision` | Choices made and reasoning |
| `preference` | How the user likes things done |
| `technical` | Technical details, configurations |
| `person` | Info about people the user mentions |
| `feedback` | Corrections, "don't do X", "always do Y" |

## Example Workflow

```
1. User says "hi, I'm working on the dashboard redesign"
2. search_memory("dashboard redesign") → find previous context
3. "Last time we discussed using Chart.js for the metrics panel..."
4. User: "Actually let's switch to D3"
5. save_memory(
     content: "Dashboard redesign: switched from Chart.js to D3 for metrics panel. User wanted more control over animations.",
     summary: "Dashboard: using D3 instead of Chart.js",
     tags: ["project", "decision"]
   )
```
