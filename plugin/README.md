# Handoff MCP — Claude Code Plugin

MCP server that gives AI coding agents persistent memory across sessions.
Tracks tasks, decisions, blockers, and project context in a local `.handoff/` directory.

## Prerequisites

Install the `handoff-mcp` binary before enabling this plugin:

```bash
# Option 1: npm (builds from source, requires Rust toolchain)
npm i -g handoff-mcp-server

# Option 2: cargo (from crates.io)
cargo install handoff-mcp
```

## What's included

- **MCP Server** — 30+ tools for session handoff, task management, metrics, scheduling, memory, and cross-project referrals
- **Skills** — handoff, handoff-load, handoff-memory, handoff-refer, handoff-import

## Getting started

After installing the plugin, initialize handoff in any project:

```
> Initialize handoff for this project
```

The agent calls `handoff_load_context` at session start and `handoff_save_context`
at session end automatically (via the bundled skills). Add this to your project's
`CLAUDE.md` for consistent behavior:

```markdown
## Session Handoff

This project uses handoff-mcp for session continuity.

- **Session start**: Call `handoff_load_context` to load previous session state.
  If not initialized, call `handoff_init` with the project name.
  If `session_guidance` is present, immediately call `handoff_save_context`
  with `session_status: "active"` to establish a persistent session before
  starting work.
- **Session end**: Call `handoff_save_context` with a summary, decisions, and blockers.
- **During work**: Use `handoff_update_task` to track progress.
  Mark tasks `in_progress` when starting, `done` when complete.
```

## Optional: Memory Hooks

Install the companion `handoff-mcp-hooks` plugin to enable automatic memory injection:

```
/plugin install handoff-mcp-hooks@handoff-mcp-marketplace
/plugin enable handoff-mcp-hooks
```

This adds hooks that run `handoff_memory_query` on every prompt and file edit,
and `handoff_memory_cleanup` at session start. Disable anytime with:

```
/plugin disable handoff-mcp-hooks
```

## VSCode Extension

For a visual UI (task explorer, dashboard, Gantt chart, Kanban board, metrics),
install the [Handoff VSCode extension](https://marketplace.visualstudio.com/items?itemName=alphaelements.handoff-vscode).

## Links

- [GitHub](https://github.com/alphaelements/handoff-mcp)
- [npm](https://www.npmjs.com/package/handoff-mcp-server)
