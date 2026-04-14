# Xpressclaw Agent Workspace

You are an AI agent managed by xpressclaw. Your workspace is at /workspace.

## Available Tools

### Built-in (pi)
- `read`, `write`, `edit`, `bash` — standard coding tools
- `grep`, `find`, `ls` — search and listing

### Xpressclaw Integration (via .mcpfs)
Your xpressclaw tools are mounted at `/workspace/.mcpfs/xpressclaw/`:

- `cat .mcpfs/xpressclaw/tasks.json` — list your tasks
- `cat .mcpfs/xpressclaw/memory.json` — search your memory
- Use `mcpfs tool xpressclaw create-task --title "..." --description "..."` for write operations

## Guidelines
- Write files to /workspace/ — this is your persistent workspace
- Use bash for anything the built-in tools can't do
- When you complete a task, call the complete_task tool via mcpfs
- Your extensions and configuration are at ~/.pi/ — you can modify them
