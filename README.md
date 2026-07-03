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

### Claude Code Plugin (recommended)

The easiest way to install handoff-mcp is as a Claude Code plugin:

```bash
# 1. Install the binary (required — the plugin calls it)
npm install -g handoff-mcp-server
# or: cargo install handoff-mcp

# 2. Add the marketplace
/plugin marketplace add alphaelements/handoff-mcp

# 3. Install the plugin (MCP server + skills)
/plugin install handoff-mcp@handoff-mcp-marketplace
```

This registers the MCP server and all skills automatically — no manual
`.mcp.json` or skill file setup needed.

**Optional: memory auto-injection hooks**

```bash
/plugin install handoff-mcp-hooks@handoff-mcp-marketplace
/plugin enable handoff-mcp-hooks
```

This adds hooks that inject relevant project memories on every prompt and file
edit. Disable anytime with `/plugin disable handoff-mcp-hooks` — the MCP server
and skills remain active.

### cargo

```bash
cargo install handoff-mcp
```

### npm

```bash
npm install -g handoff-mcp-server
```

Both install the same binary. `cargo install` fetches the crate from
[crates.io](https://crates.io/crates/handoff-mcp) and compiles it directly;
`npm install` downloads the source and runs `cargo build --release` via
postinstall, so either way you need a Rust toolchain.

### Build from source

```bash
git clone https://github.com/alphaelements/handoff-mcp.git
cd handoff-mcp
cargo build --release
```

## Setup (non-plugin)

If you installed via cargo/npm (without the plugin), register handoff-mcp as an
MCP server in Claude Code manually:

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

### Enable automatic memory injection (optional)

If you installed via the plugin, use `handoff-mcp-hooks` instead (see above).

For non-plugin installs, run:

```bash
handoff-mcp setup
```

This installs Claude Code hooks into `~/.claude/settings.json` that
automatically call `handoff_memory_query` on every prompt and file edit, and run
`handoff_memory_cleanup` at session start. Restart Claude Code after running setup.

You can check the current status or remove the hooks:

```bash
handoff-mcp setup --check      # Show hook status
handoff-mcp setup --uninstall  # Remove handoff hooks
```

The hooks fire on every prompt and file edit, which adds a small overhead per
interaction. If you want to stop automatic injection, run
`handoff-mcp setup --uninstall` — the memory tools themselves remain available
for manual use, only the automatic hooks are removed.

See [Automatic injection via hooks](#automatic-injection-via-hooks) for the
manual configuration alternative.

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
| `handoff_update_task` | Create, update, or move tasks; supports `notes_append` for safe incremental notes |
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

### Timer Coordination

| Tool | Purpose |
|------|---------|
| `handoff_timer_start` | Start tracking time for a task — delegates to VSCode extension if alive, otherwise starts MCP fallback timer |
| `handoff_timer_stop` | Stop the timer and log elapsed hours to `actual_hours` — delegates to VSCode if alive |
| `handoff_timer_get_time` | Get current timer state (elapsed, authority, projected total) without stopping |

### Project Memory

| Tool | Purpose |
|------|---------|
| `handoff_memory_save` | Save a durable project memory (lesson/rule/convention/gotcha); detects exact and near-duplicate memories and hands near-duplicates back for AI-driven merge |
| `handoff_memory_query` | Return the memories most relevant to the current prompt/file (BM25 + scope-path boost); with a `session_id`, suppresses repeats already injected this session |
| `handoff_memory_delete` | Delete a memory by id (full id or unique prefix) |
| `handoff_memory_cleanup` | Housekeeping (for SessionStart): silently merge exact duplicates, return near-duplicate/stale recommendations, gc old injection sidecars |

For usage best practices (granularity, scope_paths, conflict handling, cleanup), see `skills/handoff-memory/SKILL.md`.
See [Project Memory](#project-memory-1) below for what it is and how to wire automatic injection.

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
timer_provider = "auto"       # "auto" | "vscode" | "mcp" | "off"
timer_authority_ttl_secs = 30 # Heartbeat freshness TTL for authority.json
timer_idle_timeout_minutes = 10 # Idle pause threshold for MCP fallback timer

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

## Project Memory

Sessions answer *"what was I doing last time?"*. **Memory** answers a longer-lived
question: *"what should every session in this project always know?"* — durable
lessons, rules, conventions, and gotchas that outlive any one session.

> **Note:** The memory tools (`handoff_memory_save`, `handoff_memory_query`, etc.) can always be
> called directly by the agent. For **automatic** injection — where relevant
> memories are surfaced on every prompt without the agent asking — you need to
> configure Claude Code hooks. See [Automatic injection via hooks](#automatic-injection-via-hooks).

Memories live in `.handoff/memory/` (one JSON file per memory, plus per-session
`injected/` sidecars). A built-in multilingual similarity engine (Japanese /
English, dictionary-free) ranks relevance and detects duplicates, all in-memory
and sub-millisecond.

### Using it directly

The agent can call the memory tools at any time:

- `handoff_memory_save` — record a memory. An exact duplicate is reported (not
  rewritten); a near-duplicate comes back as a `conflict` with both bodies so the
  agent can merge them (`merge_into=<id>`, `absorb_ids=[…]`) or save separately
  with `force=true`. **handoff-mcp never merges for you** — it surfaces both
  bodies and lets the agent decide.
- `handoff_memory_query` — fetch the memories most relevant to some text and/or files.
- `handoff_memory_delete` / `handoff_memory_cleanup` — prune and de-duplicate the store.

### Automatic injection via hooks

MCP is request/response — the server cannot push a memory into the agent's
context on its own. **Claude Code hooks** close that gap: they fire regardless of
what the agent intends, call `handoff_memory_query`, and inject the matching memories as
`additionalContext`. A per-session diff (keyed on the hook `session_id`) ensures
the same memory is **not injected twice in one session** — and an *edited* memory
(new content hash) is re-injected.

| Event | Calls | Effect |
|-------|-------|--------|
| `UserPromptSubmit` | `handoff_memory_query` (prompt text) | Inject memories relevant to the prompt |
| `PreToolUse` (`Edit\|Write\|MultiEdit`) | `handoff_memory_query` (file path) | Inject memories scoped to the file being edited |
| `SessionStart` | `handoff_memory_cleanup` | Merge exact duplicates, gc old sidecars |

> **Wire hooks in your *user/global* settings, not in the repo.** Hooks are a
> personal workflow choice; the handoff-mcp repo does not ship a `.claude/`
> hooks config, and you should not commit one into a shared project. Put the
> config in `~/.claude/settings.json` (global) or your own
> `.claude/settings.local.json` (git-ignored).

**Native `mcp_tool` hook (preferred).** Recent Claude Code versions can call an
MCP tool from a hook directly, with no wrapper script. In
`~/.claude/settings.json`:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [ {
        "type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query",
        "input": { "project_dir": "${cwd}",
                   "session_id": "${session_id}", "text": "${prompt}" }
      } ] }
    ],
    "PreToolUse": [
      { "matcher": "Edit|Write|MultiEdit", "hooks": [ {
        "type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query",
        "input": { "project_dir": "${cwd}",
                   "session_id": "${session_id}", "tool_name": "${tool_name}",
                   "text": "${tool_input.file_path}",
                   "file_paths": ["${tool_input.file_path}"] }
      } ] }
    ],
    "SessionStart": [
      { "hooks": [ {
        "type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_cleanup",
        "input": { "project_dir": "${cwd}" }
      } ] }
    ]
  }
}
```

(`server` must match the name you registered handoff-mcp under — `handoff` in the
[Setup](#setup) examples.)

**Wrapper script fallback.** If your Claude Code version doesn't support the
`mcp_tool` hook type, use the bundled `command` wrapper
[`scripts/handoff-memory-hook.py`](scripts/handoff-memory-hook.py). It reads the
hook JSON on stdin, calls the server over JSON-RPC, and emits
`additionalContext` — the memory tools return their payload as a JSON *string* so
both paths parse it identically. Point all three hooks at it:

```json
{
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command",
        "command": "/path/to/handoff-mcp/scripts/handoff-memory-hook.py" } ] }
    ],
    "PreToolUse": [
      { "matcher": "Edit|Write|MultiEdit", "hooks": [ { "type": "command",
        "command": "/path/to/handoff-mcp/scripts/handoff-memory-hook.py" } ] }
    ],
    "SessionStart": [
      { "hooks": [ { "type": "command",
        "command": "/path/to/handoff-mcp/scripts/handoff-memory-hook.py" } ] }
    ]
  }
}
```

The script resolves the `handoff-mcp` binary from `PATH` (override with
`HANDOFF_MCP_BIN`) and **fails safe**: on any error it prints nothing and exits
0, so a memory miss is silent and never blocks your prompt.

### Memory settings

All under `[settings]` in `.handoff/config.toml`, all with safe defaults
(existing projects need no change), all settable via `handoff_update_config`:

| Key | Default | Meaning |
|-----|---------|---------|
| `memory_enabled` | `true` | Master switch. When `false`, all four memory tools return a benign empty result and write nothing |
| `memory_dup_threshold` | `0.72` | Jaccard similarity at/above which a save is a near-duplicate conflict and cleanup groups a cluster |
| `memory_query_min_score` | `0.5` | BM25 relevance floor for `handoff_memory_query` results |
| `memory_query_limit` | `5` | Max memories returned per query |
| `memory_stale_days` | `60` | Days without a reference before a memory is flagged stale |
| `memory_injected_gc_days` | `14` | Age at which per-session injection sidecars are garbage-collected |

## CLI API

Since v0.15.0, every MCP tool is also callable directly from the shell:

```bash
handoff-mcp <group> <action> [--key value ...]
```

All output is JSON on stdout, suitable for scripting and programmatic use
(e.g. `child_process.execFile` from a VSCode extension).

**Examples:**

```bash
# Memory operations
handoff-mcp memory save --text "Always use atomic_write" --kind lesson --tags safety,io
handoff-mcp memory query --text "atomic" --limit 5
handoff-mcp memory delete --id m-20260630-...

# Task management
handoff-mcp task list --status-filter todo
handoff-mcp task update --id t1 --title "New task" --status todo --estimate-hours 2
handoff-mcp task log-time --task-id t1 --hours 0.5

# Session and metrics
handoff-mcp session load
handoff-mcp metrics
handoff-mcp dashboard
```

**Available groups:** `init`, `task`, `session`, `config`, `memory`,
`referral`, `assignee`, `milestone`, `calendar`, `labels`, `project`,
`metrics`, `capacity`, `schedule`, `dashboard`, `timer`.

Run `handoff-mcp --help` to see all groups, or `handoff-mcp <group> --help`
for actions within a group. See the
[CLI API Reference](https://github.com/alphaelements/handoff-mcp/wiki/CLI-API-Reference)
on the wiki for the full command list.

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
- **Timer**: Use `handoff_timer_start` / `handoff_timer_stop` to track task time.
  When the VSCode extension is running, the timer delegates to it automatically.
  When the extension is absent, MCP runs a fallback timer and logs hours on stop.
  Use `handoff_timer_get_time` to check elapsed time without stopping.
- **Project memory**: Use `handoff_memory_save` to record durable lessons, rules,
  conventions, and gotchas that every future session should know. Use
  `handoff_memory_query` to retrieve relevant memories. Near-duplicate memories are
  surfaced as conflicts for you to merge or force-save — never merged silently.
```

## Skills

This repository includes skill files that make handoff behavior automatic in Claude Code:

| Skill | Purpose |
|-------|---------|
| `handoff` | Core session lifecycle, task management, metrics, scheduling |
| `handoff-load` | Quick session-start procedure |
| `handoff-memory` | Memory CRUD, conflict handling, cleanup |
| `handoff-refer` | Cross-project referrals |
| `handoff-import` | Bulk import from documents |

**Plugin users**: all skills are included automatically.

**Manual setup**: copy the skills to your user skills directory:

```bash
cp -r skills/* ~/.claude/skills/
```

## Compatibility

- **Claude Code** — fully supported (stdio transport)
- **Other MCP clients** — any client supporting the MCP stdio transport

## License

MIT
