---
name: handoff
description: "Session handoff — load context at start, save at end, track tasks during work. Triggers on session start, session end, task tracking, or when the user says 'handoff', 'save context', 'load context', 'what was I working on', or 'resume'."
---

# Handoff Skill

## Session Start

1. Call `handoff_load_context` (uses current working directory).
2. If the project is not initialized, call `handoff_init` with the project name
   derived from the directory name.
3. Review the returned context:
   - **Tasks**: check `in_progress` and `blocked` items first.
   - **Decisions**: note confidence levels — `unverified` items may need revisiting.
   - **Blockers**: address these before starting new work.
   - **Handoff notes**: pay attention to `caution` items.
   - **Context pointers**: open the referenced files to rebuild mental context.
4. Briefly summarize the current state to the user.

## During Work

- When starting a task, call `handoff_update_task` to set status to `in_progress`.
- When completing a task, update it with all `done_criteria` set to `checked: true`
  and status `done` in a single call. The server enforces that all criteria must be
  checked before accepting a `done` transition — omitting them causes an error.
- When a task is blocked, set status to `blocked` with notes explaining why.
- Create new tasks as work is discovered. Always include `done_criteria` with
  verifiable items so completion can be tracked.
- Record decisions using `handoff_save_context` with the `decisions` field
  when significant choices are made.

## Session End

When the user ends the session (or says "save context", "handoff", etc.):

1. Call `handoff_save_context` with:
   - `summary`: one sentence describing what was accomplished.
   - `decisions`: key decisions made, each with `reason` and `confidence`.
   - `blockers`: anything preventing progress.
   - `handoff_notes`: things the next session should know, categorized as
     `caution` (risks), `context` (background), or `suggestion` (ideas).
   - `context_pointers`: files and line ranges the next session should read.
2. Confirm to the user that context has been saved.
