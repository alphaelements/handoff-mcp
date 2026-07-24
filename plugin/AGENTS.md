# Session Handoff (handoff-mcp)

This project uses handoff-mcp for session continuity.
The `handoff` MCP server is registered — call its tools directly.

- **Session start**: Call `handoff_load_context` (no args — uses cwd).
  If not initialized, call `handoff_init` with the project name.
  If `session_guidance` is present, call `handoff_save_context` with
  `session_status: "active"` to establish a session before starting work.
- **Session end**: Call `handoff_save_context` with `session_status: "closed"` and:
  - `summary` — one-line description of what was accomplished
  - `decisions` — key decisions (`decision`, `reason`, `confidence`:
    `confirmed`/`estimated`/`unverified`)
  - `blockers` — anything preventing progress
  - `handoff_notes` — notes for the next session (`category`:
    `caution`/`context`/`suggestion`); include at least one `suggestion`
  - `context_pointers` — files the next session should open first
  - `related_task_ids` — task IDs this session worked on
- **During work**:
  - `handoff_update_task` — set status `in_progress` when starting, `done` when complete
  - `handoff_check_criterion` — check off `done_criteria` items as verified
- **Project memory**:
  - `handoff_memory_save` — record lessons, rules, conventions, gotchas as learned
  - `handoff_memory_query` — recall relevant memories before starting work
- **Timer**: `handoff_timer_start` / `handoff_timer_stop` (both take `task_id`)
