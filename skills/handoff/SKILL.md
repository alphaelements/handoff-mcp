---
name: handoff
description: "Session handoff — load context at start, save at end, track tasks during work. Triggers on session start, session end, task tracking, or when the user says 'handoff', 'save context', 'load context', 'what was I working on', or 'resume'."
---

# Handoff Skill

## Session Start

1. Call `handoff_load_context` (uses current working directory).
2. If the project is not initialized, call `handoff_init` with the project name
   derived from the directory name.
3. If `paused_sessions` are returned, show them to the user (ID, summary,
   branch). To resume: `handoff_load_context(session_id: "s-...")`.
   To discard: `handoff_save_context(close_session_id: "s-...")`.
4. **Establish an active session immediately** — if `session_guidance` is
   present in the response (meaning no active session exists), call
   `handoff_save_context` with `session_status: "active"` **before starting
   any work**. Include inherited context from the previous session:
   - `summary`: use `session_guidance.suggested_fields.summary` or write
     your own based on the previous session's summary
   - `decisions`, `context_pointers`, `references`: carry forward from
     `session_guidance.suggested_fields` if available
   - `handoff_notes`: at minimum, a `suggestion` noting what you plan to do
   - `checklist`: at minimum, one item noting session establishment
   This ensures that if the conversation is interrupted, the session state
   (what was being worked on, inherited decisions, file pointers) survives.
5. Review the returned context:
   - **Suggestions first**: `suggestion` handoff_notes (from current session
     or `previous_session`) are the previous session's recommended next
     actions. Unless the user's request contradicts them, start executing
     from the first suggestion — do NOT re-verify work that the suggestion
     says is already done.
   - **Tasks**: check `in_progress` and `blocked` items first.
   - **Decisions**: note confidence levels — `unverified` items may need revisiting.
   - **Blockers**: address these before starting new work.
   - **Handoff notes**: pay attention to `caution` items.
   - **Context pointers**: read these to rebuild mental context, but do NOT
     re-run tests or checks that the previous session already confirmed
     unless there are new changes since that session's commit.
6. Briefly summarize the current state to the user and start working
   immediately from the suggestion — do not repeat completed verification.

## During Work

### Task Status Management
- When starting a task, call `handoff_update_task` to set status to `in_progress`.
- When completing a task, update it with all `done_criteria` set to `checked: true`
  and status `done` in a single call. The server enforces that all criteria must be
  checked before accepting a `done` transition — omitting them causes an error.
- When a task is blocked, set status to `blocked` with notes explaining why.
- **When work reaches a point requiring user confirmation** (e.g. "push this?",
  "approve this design?"), set the task status to `review`. This signals to the
  user that their input is needed before proceeding.
- Create new tasks as work is discovered. Always include `done_criteria` with
  verifiable items so completion can be tracked.

### Progressive done_criteria Checking
- **Check off `done_criteria` immediately as each item is verified** — do not
  wait until the entire task is finished. Use `handoff_check_criterion` to
  toggle individual items:
  - Code written → check the implementation criterion
  - Tests pass → check the test criterion
  - Lint clean → check the lint criterion
  - Real-run verified → check the verification criterion
- This ensures that if the session is interrupted, the next session knows
  exactly which criteria are already satisfied.
- **done_criteria must cover the full verification chain**, not just implementation:
  1. **Implementation**: the code/config/doc changes themselves
  2. **Automated checks**: tests pass, linter/formatter clean
  3. **Real-run verification**: the change works in an actual execution
     environment (app runs, endpoint returns expected response, UI renders
     correctly, CLI produces correct output, etc.)
  - A task is not done until verified end-to-end by running the real
    artifact — passing automated checks alone is insufficient.

### Progressive Session Updates
- Use `handoff_update_session` to incrementally update the active session
  during work — no need to call `save_context` for small updates:
  - **Toggle session checklist items**: `checklist_index` + `checklist_checked`
  - **Add decisions as they happen**: `add_decision`
  - **Add context pointers** to files you've been working on: `add_context_pointer`
  - **Add handoff notes** (cautions, context): `add_handoff_note`
- Record decisions as they are made, not just at session end.
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
   - **Do not repeat task completion status** — the task system already
     tracks what is done via `done_criteria`. Reference task IDs instead.
     Bad: "All 138 tests pass and clippy is clean. Next: push branch"
     Good: "Next: push branch and create MR (see t7 done_criteria)"
   - If the next work belongs to a **different project**, say so explicitly
     (e.g. "Next work is in handoff-vscode, not this project").

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
     Point to files the next session **needs to work on or understand**,
     not files that are already complete. If a file was changed and is done,
     mention it in a `context` handoff_note instead.
     The server warns if empty.
   - `decisions`: the server warns if empty.
   - `references`: relevant docs, issues, MRs. The server warns if empty.

   By default, `save_context` writes the handoff data into the active
   session and closes it (`.active.json` → `.closed.json`). With
   `session_status: "active"`, it keeps the session active instead of
   closing it — use this at session start to establish a persistent
   session, and omit it (or use the default `"closed"`) at session end.

4. **Review the server response** for warnings:
   - If the server warns about unchecked checklist items, either check
     them (if done) or acknowledge them to the user.
   - If the server warns about missing suggestions, add suggestion notes
     and re-save.
   - If the server warns about missing context_pointers, decisions, or
     references, add them if applicable.

5. Confirm to the user that context has been saved.
