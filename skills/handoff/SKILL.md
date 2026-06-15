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
- **done_criteria must cover the full verification chain**, not just implementation:
  1. **Implementation**: the code/config/doc changes themselves
  2. **Automated checks**: tests pass, linter/formatter clean
  3. **Real-run verification**: the change works in an actual execution
     environment (app runs, endpoint returns expected response, UI renders
     correctly, CLI produces correct output, etc.)
  - A task is not done until verified end-to-end by running the real
    artifact — passing automated checks alone is insufficient.
- Record decisions using `handoff_save_context` with the `decisions` field
  when significant choices are made.
- **Before session end, review the overall plan**: call `handoff_list_tasks`
  to see the full picture, then enumerate the next phase's steps as
  `suggestion` handoff_notes. This ensures continuity across sessions.

### Time Tracking (handoff-vscode F9)

When the handoff-vscode time tracker is enabled, `schedule.actual_hours`
is updated automatically by the VSCode extension. AI sessions should:

- **Not overwrite `actual_hours` blindly** — the time tracker accumulates
  values; use `logTime` (additive) rather than setting `actual_hours`
  directly.
- **Set `schedule.estimate_hours`** on task creation so the tracker can
  show estimate vs actual progress.
- At session end, if the time tracker was running, the extension
  auto-stops and logs the elapsed time. The AI does not need to log
  time manually for tasks tracked by the extension.

## Session End

When the user ends the session (or says "save context", "handoff", etc.):

1. **Review the overall plan** before saving:
   - Call `handoff_list_tasks` to see the current task tree.
   - Identify which tasks were completed, which remain, and what the
     logical next phase of work is.
   - If the original plan needs adjustment based on what was learned,
     note the changes in `decisions`.

2. **Write actionable next-step suggestions**:
   - Add at least one `handoff_notes` entry with `category: "suggestion"`
     that describes a **concrete first action** for the next session
     (not vague guidance like "continue working" — instead: "Run
     `cargo test` on the new validation, then implement the wiki spec
     update per the plan in t7").
   - List the next 2-3 steps the next session should take, in priority
     order, as separate `suggestion` entries.

3. Call `handoff_save_context` with:
   - `summary`: one sentence describing what was accomplished.
   - `decisions`: key decisions made, each with `reason` and `confidence`.
   - `blockers`: anything preventing progress.
   - `checklist`: items for the next session or user to verify. Mark
     completed items as `checked: true` before saving. The server warns
     if unchecked items remain or if checklist is empty.
   - `handoff_notes`: things the next session should know, categorized as
     `caution` (risks), `context` (background), or `suggestion` (next
     actions). **At least one `suggestion` is required** — the server
     warns if none is provided.
   - `context_pointers`: files and line ranges the next session should read.
     The server warns if empty.
   - `decisions`: the server warns if empty.
   - `references`: relevant docs, issues, MRs. The server warns if empty.

4. **Review the server response** for warnings:
   - If the server warns about unchecked checklist items, either check
     them (if done) or acknowledge them to the user.
   - If the server warns about missing suggestions, add suggestion notes
     and re-save.
   - If the server warns about missing context_pointers, decisions, or
     references, add them if applicable.

5. Confirm to the user that context has been saved.
