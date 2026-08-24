## Session Handoff

This project uses handoff-mcp for session continuity.

- **Session start**: `handoff_load_context` → if not initialized, `handoff_init`.
  If `session_guidance` is present, `handoff_save_context(session_status:"active")`
  before starting work.
- **Session end**: `handoff_save_context` with summary, decisions, blockers.
- **During work**: `handoff_update_task` — mark `in_progress` on start, `done` on complete.

### Multi-Worktree

Sub-worktrees automatically share the primary WT's `.handoff/` via symlink.
No special setup needed — `handoff_load_context` in a sub-WT handles it.
Use `handoff_overview` from the primary WT to see cross-WT progress.

### `.handoff/` Repository Management

`.handoff/` should be managed as an independent git repo, not tracked by the
project repo. Add `/.handoff/` to the project's `.gitignore`, then `git init`
inside `.handoff/`. When committing project changes, also commit `.handoff/`
state in its own repo to keep session history in sync.
