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

Register handoff-mcp as an MCP server in Claude Code:

**Option A** — CLI (recommended):

```bash
claude mcp add -s user handoff -- handoff-mcp
```

The `-s user` flag registers it globally (available in all projects). Verify with `claude mcp get handoff`.

**Option B** — Manual edit of `~/.claude.json`:

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

### Core Session Management

| Tool | Purpose |
|------|---------|
| `handoff_init` | Initialize `.handoff/` directory for a project |
| `handoff_load_context` | Load session context, tasks, and git state at session start |
| `handoff_save_context` | Save session state — establish an active session or close it with handoff data |
| `handoff_update_session` | Incrementally update active session (toggle checklist, add decisions/notes/pointers) |
| `handoff_list_sessions` | List all sessions (open/active/paused/closed) with summary info |
| `handoff_get_session` | Get full detail of a specific session by ID |

### Task Management

| Tool | Purpose |
|------|---------|
| `handoff_list_tasks` | List tasks with filters (status, assignee, milestone, priority, label) |
| `handoff_get_task` | Get full task details (notes, done_criteria, schedule, etc.) |
| `handoff_update_task` | Create, update, or move tasks in a hierarchical tree |
| `handoff_check_criterion` | Toggle a single done_criteria item by index |
| `handoff_log_time` | Log hours worked — adds to `actual_hours`, deducts from `remaining_hours` |
| `handoff_bulk_update_tasks` | Update multiple tasks in one call (status, schedule, assignee, priority) |

### Metrics & Scheduling

| Tool | Purpose |
|------|---------|
| `handoff_get_metrics` | Project metrics: completion %, effort, overdue, budget, milestones |
| `handoff_get_capacity` | Work capacity for a date range, respecting calendar and assignee config |
| `handoff_auto_schedule` | Auto-schedule tasks based on dependencies, estimates, and capacity |

### Configuration & Team

| Tool | Purpose |
|------|---------|
| `handoff_get_config` | Read project configuration (full TOML as JSON) |
| `handoff_update_config` | Update config: settings, calendar, assignees, effort budget, gantt view |
| `handoff_list_assignees` | List team members with task counts and effort stats |
| `handoff_add_assignee` | Add a team member (`[assignees.<key>]`) |
| `handoff_update_assignee` | Update a team member's fields (partial; null clears a field) |
| `handoff_remove_assignee` | Remove a team member and unassign them from every task |
| `handoff_list_milestones` | List milestones (`[milestones.*]`) |
| `handoff_add_milestone` | Add a milestone (date, color, description) |
| `handoff_update_milestone` | Update a milestone (partial) |
| `handoff_remove_milestone` | Remove a milestone |
| `handoff_update_calendar` | Patch the project `[calendar]` (work hours, closed days, `day_hours`, schedule_mode) |
| `handoff_update_labels` | Set the project-level label vocabulary |
| `handoff_start_project` | Set `started_at` and optionally shift all task dates to the project start |

These CRUD tools and the VSCode extension write the same `config.toml`, so the
GUI and the MCP server stay in full parity. All writes are atomic (temp-file +
rename) so a concurrent reader never sees a partially-written file.

### Cross-Project

| Tool | Purpose |
|------|---------|
| `handoff_dashboard` | Overview of all handoff-enabled projects |
| `handoff_import_context` | Bulk import tasks and session data from documents |
| `handoff_refer` | Send a cross-project referral (bug, improvement, request) |
| `handoff_list_referrals` | List incoming referrals from other projects (summaries only) |
| `handoff_get_referral` | Fetch one incoming referral in full — details, suggested tasks, done_criteria, context |
| `handoff_update_referral` | Update referral status (open → acknowledged → resolved) |

### Task Data Model

Tasks are stored as a directory tree with status encoded in filenames:

```
tasks/
├── t1-implement-auth/
│   ├── _task.done.json
│   ├── t1.1-design-schema/
│   │   └── _task.done.json
│   └── t1.2-write-handlers/
│       └── _task.in_progress.json
└── t2-deploy-staging/
    └── _task.blocked.json
```

Statuses: `todo` | `in_progress` | `review` | `done` | `blocked` | `skipped`

Each task can have:
- **Assignee** — team member key (matches `[assignees.<key>]` in config.toml)
- **Priority** — `low` / `medium` / `high`
- **Labels** — free-form tags
- **Done criteria** — checklist items (all must be checked before `done` transition)
- **Links** — URLs to issues, MRs, or docs
- **Notes** — markdown description
- **Schedule** — `start_date`, `due_date`, `estimate_hours`, `actual_hours`, `remaining_hours`, `milestone`, `pinned`
- **Dependencies** — task IDs this task depends on (circular deps rejected)

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
history_limit = 20            # Max closed sessions to keep
done_task_limit = 10          # Max completed tasks to show
auto_git_summary = true       # Capture git state automatically
require_estimate_hours = true # Require estimate_hours on leaf tasks (default true)
ai_estimate_multiplier = 0.2  # Multiplier turning human estimates into AI-effort hours

[dashboard]
scan_dirs = ["~/pro/"]     # Directories to scan for dashboard

[calendar]
work_hours_per_day = 8
closed_weekdays = ["sat", "sun"]
closed_dates = ["2026-12-25"]
open_dates = []
schedule_mode = "auto"     # "auto" or "manual"
overwork_limit_percent = 150

[calendar.day_hours]
fri = 4                    # Per-weekday hour overrides

[effort_budget]
total_hours = 500          # Total project effort cap

[assignees.alice]
display_name = "Alice Chen"
color = "#4A90D9"
work_hours_per_day = 8
closed_weekdays = [1, 2]   # Per-assignee overrides

[assignees.bob]
display_name = "Bob Martinez"
color = "#E74C3C"
work_hours_per_day = 6

[gantt_view]
sort = "start"             # start, id, id-desc, status
zoom = "week"              # day, week, month
mode = "compare"           # plan, actual, compare
```

All configuration sections can be updated via `handoff_update_config` with dot-notation keys (e.g., `"calendar.work_hours_per_day": 7`).

### Estimates and AI effort

handoff-mcp distinguishes the **raw human-effort estimate** you record on a task
from the **AI-effort hours** used in scheduling and metrics:

- **`require_estimate_hours`** (default `true`) — `handoff_update_task` rejects
  creating or updating a *leaf* task (in `todo` / `in_progress` / `review` /
  `done`) without `schedule.estimate_hours > 0`. Parent tasks (with children) and
  `blocked` / `skipped` tasks are exempt, and an estimate already on the task
  satisfies the requirement. Set to `false` to opt out.
- **`ai_estimate_multiplier`** (default `0.2`) — the factor applied to raw
  estimates to model how long the work takes when an AI agent does it. Always
  record the *raw human-effort* estimate; the multiplier is applied at
  aggregation time by `handoff_get_metrics` (`total_adjusted_estimate_hours` and
  per-milestone `adjusted_estimate_hours`) and `handoff_get_capacity`. Raw values
  are never overwritten.

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
