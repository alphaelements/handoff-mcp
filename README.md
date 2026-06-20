# handoff-mcp

An MCP server that gives AI coding agents persistent memory across sessions.

When you close a Claude Code session and start a new one, the new session has no idea what the previous one was doing. handoff-mcp solves this by saving session context — tasks, decisions, blockers, and file pointers — to a local `.handoff/` directory that the next session can load automatically.

## The Problem

AI coding sessions are stateless. Every new session starts from zero:

- **"What was I working on?"** — the agent doesn't know
- **"What decisions were made?"** — lost with the previous context window
- **"What's left to do?"** — you have to re-explain everything

This gets painful fast on multi-session projects.

## How It Works

```
Session 1                          Session 2
┌──────────────┐                   ┌──────────────┐
│ Working...   │   .handoff/       │ load_context │
│              │──────────────────>│  ↓ guidance  │
│ save_context │   tasks/          │ save_context │
│  - close     │   sessions/      │  (active)    │
│  - summary   │   config.toml    │  ↓ work...   │
│  - decisions │                   │ save_context │
│  - blockers  │                   │  (close)     │
└──────────────┘                   └──────────────┘
```

At session start, the agent calls `handoff_load_context` to pick up where things left off. If no active session exists, the response includes `session_guidance` prompting the agent to establish one via `handoff_save_context` with `session_status: "active"` — this creates a persistent `.active.json` that survives interruptions. At session end, the agent calls `handoff_save_context` (defaulting to `session_status: "closed"`) to close the session.

## Installation

### npm (recommended)

```bash
npm install -g handoff-mcp-server
```

### Build from source

```bash
git clone https://github.com/alphaelements/handoff-mcp.git
cd handoff-mcp
cargo build --release
```

## Setup

Add to your Claude Code MCP configuration:

**Global** (`~/.claude/.mcp.json`) — available in all projects:

```json
{
  "mcpServers": {
    "handoff": {
      "type": "stdio",
      "command": "handoff-mcp",
      "args": []
    }
  }
}
```

**Per-project** (`.mcp.json` in project root):

```json
{
  "mcpServers": {
    "handoff": {
      "type": "stdio",
      "command": "handoff-mcp",
      "args": []
    }
  }
}
```

## Quick Start

1. **Initialize** a project:

   The agent calls `handoff_init` with your project name. This creates a `.handoff/` directory:

   ```
   .handoff/
   ├── config.toml      # Project settings
   ├── sessions/        # Session history (TOML files)
   └── tasks/           # Task tree (directories + TOML files)
   ```

2. **Load context** at session start — the agent calls `handoff_load_context`. If `session_guidance` is returned, the agent establishes an active session via `handoff_save_context` with `session_status: "active"` before starting work.

3. **Work normally** — create tasks, track progress, make decisions. The active session persists on disk, so progress survives interruptions.

4. **Save context** at session end — the agent calls `handoff_save_context` to close the active session with handoff data (summary, decisions, blockers, references).

> Add `.handoff/` to your `.gitignore` — it contains local working state, not code.

## Tools

| Tool | Purpose |
|------|---------|
| `handoff_init` | Initialize `.handoff/` directory for a project |
| `handoff_load_context` | Load session context, tasks, and git state at session start |
| `handoff_save_context` | Save session state — establish an active session (`session_status: "active"`) or close it (default) with handoff data |
| `handoff_list_tasks` | List tasks with optional status filter |
| `handoff_update_task` | Create, update, or move tasks in a hierarchical tree |
| `handoff_get_config` | Read project configuration |
| `handoff_update_config` | Update project configuration |
| `handoff_dashboard` | Overview of all handoff-enabled projects |

### Task Management

Tasks are stored as a directory tree, supporting hierarchical structures:

```
tasks/
├── 01-todo--implement-auth/
│   ├── task.toml
│   ├── 01.1-done--design-schema/
│   │   └── task.toml
│   └── 01.2-in_progress--write-handlers/
│       └── task.toml
└── 02-blocked--deploy-staging/
    └── task.toml
```

Statuses: `todo` | `in_progress` | `review` | `done` | `blocked` | `skipped`

Each task can have:
- Priority (`low` / `medium` / `high`)
- Labels
- Done criteria (checklist items)
- Links to issues, MRs, or docs
- Notes

### Session Context

When saving context, the agent can record:

- **Summary** — one-line description of what happened
- **Decisions** — what was decided and why, with confidence levels (`confirmed` / `estimated` / `unverified`)
- **Blockers** — what's preventing progress
- **Checklist** — items for the next session
- **Handoff notes** — categorized as `caution`, `context`, or `suggestion`
- **References** — links to files, issues, MRs, wiki pages, or URLs
- **Context pointers** — specific files and line ranges the next session should look at
- **Git state** — current branch, recent commits, and dirty files (captured automatically)

### Dashboard

`handoff_dashboard` scans directories for projects with `.handoff/` and shows a summary:

```
## my-project (3 tasks)
  - [in_progress] Implement auth (high)
  - [todo] Add tests (medium)
  - [blocked] Deploy staging (medium)

## other-project (1 task)
  - [review] Update README (low)
```

## Configuration

`.handoff/config.toml`:

```toml
[project]
name = "my-project"
description = "Project description"

[settings]
history_limit = 20         # Max closed sessions to keep
done_task_limit = 10       # Max completed tasks to show
auto_git_summary = true    # Capture git state automatically

[dashboard]
scan_dirs = ["~/pro/"]     # Directories to scan for dashboard
```

## MCP Resources

| URI | Description |
|-----|-------------|
| `handoff://sessions` | Active session data (JSON) |
| `handoff://config` | Project configuration (TOML) |

## Recommended CLAUDE.md Setup

Add the following to your project's `CLAUDE.md` so the agent uses handoff consistently:

```markdown
## Session Handoff

This project uses handoff-mcp for session continuity.

- **Session start**: Call `handoff_load_context` to load previous session state.
  If not initialized, call `handoff_init` with the project name.
  If `session_guidance` is present, immediately call `handoff_save_context`
  with `session_status: "active"` to establish a persistent session before
  starting work. Include inherited context from the previous session.
- **Session end**: Call `handoff_save_context` with a summary, decisions, and blockers.
- **During work**: Use `handoff_update_task` to track progress.
  Mark tasks `in_progress` when starting, `done` when complete.
- **Decisions**: Record decisions with confidence levels as they are made,
  not just at session end. Use `confirmed` for verified facts, `estimated`
  for reasonable assumptions, `unverified` for unknowns.
```

## Skill File (Optional)

This repository includes a skill file at [`skills/handoff/SKILL.md`](skills/handoff/SKILL.md) that makes handoff behavior automatic in Claude Code. Copy it to your user skills directory:

```bash
cp -r skills/handoff ~/.claude/skills/
```

This teaches the agent to automatically load context at session start, track tasks during work, and save context at session end.

## Compatibility

- **Claude Code** — fully supported (stdio transport)
- **Other MCP clients** — any client supporting the MCP stdio transport

## License

MIT
