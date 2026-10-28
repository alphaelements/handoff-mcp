---
name: handoff
description: "Session handoff — load context at start, save at end, track tasks during work. Triggers on session start, session end, task tracking, or when the user says 'handoff', 'save context', 'load context', 'what was I working on', 'resume', 'milestone', 'frozen', 'マイルストーン', 'リリーススコープ', 'depends_on', 'milestone report', 'capacity', 'metrics', 'timer', 'report', 'calendar', 'labels', 'bulk_update', or 'events'."
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
   - **Project health**: `trace_health` (V-model coverage, suspects),
     `suspect_summary`, `requirements_health` and `docs_health` carry
     structured warnings (e.g. `DIAG-R001`). `has_data: false` (and
     `suspect_summary: null`) means the V-model is not in use yet — see the `handoff-trace` skill's
     "First-time setup" if the user wants it.
6. Briefly summarize the current state to the user and start working
   immediately from the suggestion — do not repeat completed verification.

## During Work

### Diagnostics & Warnings Handling

Every `handoff_*` tool's response may carry a `warnings` array. Each entry is
either a plain string (legacy/simple cases) or a structured object —
`{severity, code, message, fix_hint?}` — and both shapes can appear in the
same array (`serde(untagged)`, so existing string-only callers keep working
unchanged). Check `warnings` after **every** tool call, not just the ones
that failed:

- **Plain string entries** — display as-is to the user; there is no
  structure to branch on beyond the text itself.
- **Structured entries** — branch on `severity`:
  - `"error"` — surface to the user immediately and stop the current step;
    treat it like a failed call even if the tool technically returned
    success (e.g. a partial/degraded result).
  - `"warning"` — report to the user and decide whether to address it before
    continuing (does not block by itself).
  - `"info"` — record in the session's working log / handoff notes; surface
    to the user only if it's relevant to what they asked for.
  - If `fix_hint` is present, present it as a concrete next action (e.g. a
    command to run or a field to set) rather than just echoing the message —
    `fix_hint` exists so the AI doesn't have to improvise a remedy.
  - `code` (e.g. `"DIAG-T001"`) is the stable machine-readable identifier —
    use it to recognize recurring diagnostics across calls/sessions instead
    of matching on the human-readable `message` text, which may be reworded.
- Tool-family-specific diagnostic codes and their meanings are documented in
  that family's own skill — see `handoff-trace/SKILL.md`'s "Diagnostics
  handling" section (`trace_*` tools) and `handoff-docs/SKILL.md`'s
  diagnostics section (`doc_req_*` tools).
- An **empty result** (e.g. an empty list where you expected items) is not
  automatically "nothing to do" — check `warnings` first; it often explains
  *why* the result is empty (missing config, no matching data, an unreadable
  source file) rather than confirming there's genuinely nothing there.

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
- **Set `schedule.estimate_hours`** (raw human-effort hours, > 0) on leaf tasks
  when moving them to `in_progress`/`review`/`done`. It is required by default —
  `handoff_update_task` rejects leaf tasks in those statuses without it. Tasks in
  `todo`/`blocked`/`skipped` and parent tasks (with children) are exempt. Enter the
  raw human-effort estimate; the AI-effort multiplier
  (`settings.ai_estimate_multiplier`, default 0.2) is applied automatically at
  aggregation time by `handoff_get_metrics`/`handoff_get_capacity`. To turn the
  requirement off, set `settings.require_estimate_hours = false`.

### Checklist before every `handoff_update_task` that creates a task

Run through this as you write the call, not after it is rejected. A leaf task
missing `estimate_hours` is refused, costing a round trip.

- [ ] `title` — present (required for any new task)
- [ ] `done_criteria` — verifiable items, not restatements of the title
- [ ] `schedule.estimate_hours` — **> 0, raw human-effort hours.** Skip only if
      the task is a parent (has children) or its status is `todo`/`blocked`/`skipped`
- [ ] `priority` — `low` / `medium` / `high`
- [ ] `labels` — at least one, so the task is findable by filter
- [ ] `assignee` — matches a key in `config.toml [assignees.<key>]`

A minimal accepted payload:

```json
{
  "task": {
    "title": "Add retry to the upload path",
    "status": "todo",
    "priority": "high",
    "labels": ["upload", "reliability"],
    "assignee": "ai",
    "schedule": { "estimate_hours": 2.0 },
    "done_criteria": [{ "item": "Upload retries 3x on 5xx, then surfaces the error" }]
  }
}
```

When updating an existing task, you do **not** resend `estimate_hours` — the
stored value satisfies the requirement. Send only the fields you are changing.

### Appending to Task Notes
- Use `notes_append` (not `notes`) in `handoff_update_task` or
  `handoff_bulk_update_tasks` to add text to existing task notes without
  replacing them. The server adds a `--- YYYY-MM-DDTHH:MM:SS` timestamp
  heading automatically.
- This is safe for multi-agent/multi-step workflows — no read-modify-write
  needed, no risk of losing prior notes.
- If both `notes` (replace) and `notes_append` are provided in the same call,
  `notes` takes precedence and `notes_append` is ignored.

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

### Time Tracking

Use `handoff_log_time` to record hours worked on a task:
- Atomically adds to `schedule.actual_hours` and deducts from `schedule.remaining_hours`.
- Prefer the additive `handoff_log_time` tool over setting `actual_hours` directly.
- If you do set `schedule.actual_hours` directly (`handoff_update_task` /
  `handoff_bulk_update_tasks`), the difference from the previous value is appended
  to `.handoff/time_log.jsonl` with `source: "direct_update"`. A decrease is
  logged as a **negative** entry (a correction), so the time series and the
  `effort` / `weekly` / `monthly` report totals (which sum entries, negatives
  included) follow `actual_hours`. An unchanged value logs nothing.
  Only those two paths log: setting `actual_hours` at task creation and
  `handoff_timer_stop` change it without a log entry, so the log can fall
  short of `actual_hours` for such tasks. Pick one way per task: do not both
  call `handoff_log_time` and write an absolute `actual_hours` (the delta would
  be logged twice).
  A direct set does not touch `remaining_hours`; `handoff_log_time` entries have
  no `source` and are always positive.
- Set `schedule.estimate_hours` on task creation (required by default) so metrics can show estimate vs actual.
- When the handoff-vscode time tracker is enabled, the extension also logs time
  automatically — the AI does not need to log time manually for extension-tracked tasks.
- `handoff_update_task`'s `schedule` field **merges** (partial update): passing
  `schedule: { milestone: "v2" }` updates only the milestone and preserves
  `actual_hours`/`remaining_hours`. It never replaces the whole schedule object.

### Timer Coordination (MCP ⇄ VSCode)

Use `handoff_timer_start` / `handoff_timer_stop` / `handoff_timer_get_time` to
track task time with automatic VSCode extension coordination:

- **`handoff_timer_start`** — if the VSCode extension is running (live authority
  heartbeat), the request is delegated via `.handoff/timer/requests/`. If absent,
  MCP starts a fallback internal timer.
- **`handoff_timer_stop`** — if delegated, creates a stop request for the extension.
  If MCP is the fallback, stops the timer and atomically logs elapsed hours to
  `actual_hours` (same as `handoff_log_time`).
- **`handoff_timer_get_time`** — reads `.handoff/timer/state.json` to show
  elapsed time, state (tracking/paused/stopped), and current authority (vscode/mcp).
- The `timer_provider` config setting controls behavior: `"auto"` (default) uses
  the authority protocol, `"vscode"` always delegates, `"mcp"` always uses
  fallback, `"off"` disables timer tools entirely.

### Brainstorming Sessions

Use the `handoff_brainstorm_*` tools to run and record a brainstorm (the first
stage before requirements):

- **`handoff_brainstorm_start`** — `topic` (required), `initial_context`,
  `project_name`. Creates the session state `.handoff/brainstorm/<brainstorm_id>.json`
  plus two layer-less docs (deliberation log, report). Returns `brainstorm_id`,
  `log_doc_id`, `report_doc_id`, `coverage`.
- **`handoff_brainstorm_turn`** — call once per dialogue turn with `brainstorm_id`,
  `topic_tag`, `summary`, optional `decisions` / `open_questions` /
  `coverage_update` / `phase`. Appends `Turn N` to the log, mirrors decisions into
  the report, and updates coverage (required topics: vision, motivation,
  target-users, core-value, scope; any other tag is tracked as an optional
  topic). A turn with `decisions` marks its topic `covered`;
  `required_covered: true` means it is time to propose the research phase.
- **`handoff_brainstorm_status`** — read-only; restores phase, turn count,
  coverage, decisions and open questions after an interruption or restart.
- **`handoff_brainstorm_finalize`** — `executive_summary` and `scope_in`
  (required) plus `scope_out`, `undecided`, `priorities`, `research_topics`,
  `requirement_hints`. Writes the final report (research results under
  `## 6. 調査結果` are preserved), marks the log complete and closes the session.

### Metrics & Project Health

Check project health with `handoff_get_metrics` at session start:
- Returns completion %, overdue tasks, budget status, milestone breakdown.
- Use `assignee` filter to scope metrics to a specific team member.
- Use metrics to prioritize work: address overdue tasks first, then blocked, then todo.
- `handoff_save_context` stores a daily metrics snapshot in `.handoff/metrics_snapshots/<YYYY-MM-DD>.json` (same-day saves overwrite); `handoff_snapshot_metrics` takes one on demand. Snapshots (schema v2) also list every task (id, title, status, assignee, priority); `handoff_get_metrics_snapshot(date)` restores the task state of a past day (exact only when that day's snapshot was not followed by later status changes that day; otherwise the nearest snapshot plus `events.jsonl` replay or an estimate, flagged `exact=false`).

### Capacity & Scheduling

Before assigning dates to tasks, check available capacity:
- `handoff_get_capacity` — shows hours available per day for a date range, respecting the calendar and assignee configs.
- `handoff_auto_schedule` — auto-computes optimal start/due dates:
  - Use `dry_run: true` (default) to preview changes without writing.
  - Use `dry_run: false` to apply computed dates to task files.
  - Respects task dependencies, pinned dates, and per-assignee calendars.
  - Respects per-day capacity overrides (`calendar.day_hours`, e.g. a half-day Friday).
  - Use `start_date: "YYYY-MM-DD"` to anchor the earliest task (defaults to today).
  - Returns a change diff showing old vs new dates for each task.
  - Milestone `depends_on` (see Milestone Management): a milestone's tasks are placed after those of every milestone it depends on, and no earlier than the day those end.
  - A `depends_on` cycle rejects the whole run before anything is written; the error names the cycle path (`a → b → a`). Fix `depends_on` with `handoff_update_milestone`.
  - The response always carries `milestone_summary` (per milestone: `milestone`, `date`, `task_count`, `total_estimate_hours`, `latest_due`, `overdue`) for the tasks scheduled in this run. A task due after its milestone `date` also adds a `warnings` entry.
  - A task without `estimate_hours` is scheduled with `settings.default_estimate_hours` (config.toml `[settings]`, default `8.0`; set via `handoff_update_config` with `settings.default_estimate_hours`, a number > 0). When any task fell back to that default, `warnings` gains one entry: `N tasks have no estimate_hours — using default Xh each. Set estimate_hours on tasks or settings.default_estimate_hours in config.toml.` No such entry appears when every scheduled task has an estimate.

### Team & Assignee Management

- `handoff_list_assignees` — lists all team members from config.toml with task counts, active task counts, and effort hours.
- `handoff_add_assignee` — add a member: `key` (required), `display_name`, `color`, `work_hours_per_day`, `closed_weekdays`, `closed_dates`, `open_dates`, `day_hours`.
- `handoff_update_assignee` — patch an existing member (only provided fields change; pass `null` to clear a field).
- `handoff_remove_assignee` — remove a member **and** unassign them from every task automatically.
- Assign tasks via `handoff_update_task` (single) or `handoff_bulk_update_tasks` (batch).

### Milestone Management

- `handoff_list_milestones` — list all milestones (`name → {date, color, description}`).
- `handoff_add_milestone` — add a milestone: `name` (required), `date`, `color`, `description`.
- `handoff_update_milestone` — patch an existing milestone (partial).
- `handoff_remove_milestone` — remove a milestone.

The lifecycle fields below are also accepted by the add/update tools: `status`, `order`, `depends_on` (plus `version`); `handoff_list_milestones` takes `status_filter`.

#### Milestone lifecycle

`planning` (default when unset) → `active` → `completed` → `frozen` → `released`. `cancelled` is allowed from any status.

- `handoff_update_milestone(status=...)` only accepts forward moves. A backward move (e.g. `frozen` → `active`) or leaving `cancelled` is rejected unless `force: true`.
- **Frozen guard**: assigning a task (`schedule.milestone`) to a `frozen` milestone still succeeds but returns a warning, which guards against scope creep. Move the task elsewhere, or unfreeze deliberately with `force: true`.
- `depends_on` lists the milestones that must finish first. A name that is undefined or the milestone itself is saved with a warning.

#### Reading the Dashboard Milestones section

`handoff_dashboard` adds a `milestones` array to a project that defines milestones (omitted otherwise), ordered by `order`. Each row has `name`, `version`, `status`, `due`, `tasks: {done, total}` (tasks whose `schedule.milestone` is this milestone) and `requirement_coverage: {total, covered, coverage_percent}` (`covered` = requirement at `implemented` or beyond; `coverage_percent` is `null`, not 0, when the milestone has no requirements). Use it to spot a frozen milestone with low task progress, or an `active` one past its `due`.

#### Milestone workflow

1. At release planning, create each milestone as `planning` (`handoff_add_milestone`, with `date`/`version`).
2. Set `depends_on` for ordering constraints between milestones.
3. When scope is settled, move it to `active`.
4. When scope is locked, move it to `frozen`; later task additions warn.
5. After shipping, move it to `released`.
6. Run `handoff_auto_schedule` (dry run) and check `milestone_summary` for overdue milestones; fix any `depends_on` cycle it reports.
7. Generate the milestone report with `handoff_report(action="generate", report_type="milestone", scope={milestone: "<name>"})`.

### Project Calendar, Labels & Start

- `handoff_update_calendar` — patch `[calendar]` in one call: `work_hours_per_day`, `closed_weekdays`, `closed_dates`, `open_dates`, `day_hours`, `schedule_mode`. Only provided fields change.
- `handoff_update_labels` — set the project-level label vocabulary (`labels` array).
- `handoff_start_project` — set `started_at` and, with `shift_dates: true`, move every task's dates so the earliest start lands on the project start date.

### Multi-session

When `multi_session = true` (default for new projects), multiple active sessions
can coexist. Use `session_id` on load/save/update to target a specific one.

- `handoff_fork_session` — branch from an existing session. Inherits decisions,
  context_pointers, references, handoff_notes by default. Sets
  `parent_session_id`. Source can be active, paused, or closed.
- `handoff_merge_sessions` — combine multiple sessions (append mode). Detects
  duplicate decisions as conflicts. Non-target sources closed by default.
- **Switch**: `handoff_save_context(pause_session_id: "s-current")` then
  `handoff_load_context(session_id: "s-target")`.
- **Session fields**: `timeline` (grouping label), `label` (short name),
  `parent_session_id` (fork origin), `related_task_ids` (task association).

### Session Browsing

- `handoff_list_sessions` — list sessions; filter by status, `timeline`;
  `include_children: true` adds child session arrays for branching visualization.
- `handoff_get_session` — get full detail of any session by ID (decisions, checklist, handoff_notes, context_pointers, references, timeline, parent_session_id).
- Use these to reference decisions or context from past sessions without needing to re-read the full session file.

### Multi-agent Coordination (claims, worktrees)

When several agents or worktrees work the same project, claim a task before
working on it so two agents don't pick the same one:

| tool | use |
|---|---|
| `handoff_claim_task(task_id, session_id?, lease_ttl?)` | exclusive claim with a lease (a todo/blocked task moves to in_progress); fails if another agent holds a live lease, takes over an expired one; warns (never blocks) when `scope_paths` overlap another active task |
| `handoff_release_task(task_id, reason?, revert_status?)` | release your own claim |
| `handoff_reclaim_task(task_id, reason?)` | management override: force-release anyone's claim, task back to todo, event recorded |
| `handoff_list_agents(status?, include_tasks?)` | registered agents across worktrees with active/stale/disconnected status |
| `handoff_overview()` | one view of agents, task × agent claims, worktree × branch × session (run from the primary worktree) |
| `handoff_events(since?, task_id?, agent_id?, event_type?, limit?)` | lifecycle log: `task.claimed` / `released` / `expired` / `reclaimed`, `agent.registered`, `session.created` / `closed` |

`handoff_dashboard(scan_dirs?, max_depth?, exclude_patterns?, include_completed?)`
summarizes every handoff project under the configured scan directories
(defaults from `config.toml [dashboard]`).

### Bulk Operations

Use `handoff_bulk_update_tasks` for:
- Applying auto-schedule results to multiple tasks.
- Batch status changes (e.g., closing all review tasks).
- Batch assignee changes (e.g., reassigning a team member's tasks).
- Each task update is independent — failures on one task don't roll back others.
- The `estimate_hours` rule applies here too: an update leaving a leaf task in
  `in_progress`/`review`/`done` without `schedule.estimate_hours` is rejected and
  reported in `errors[]`. Supply the estimate in the same update when moving a
  task out of `todo`/`blocked`/`skipped`. Parent tasks and the statuses
  `todo`/`blocked`/`skipped` are exempt.

### Document Management

For structured, multi-section documents (specs, designs, ADRs, guides) that
are too large for a single memory entry, use the doc tools instead — see the
`handoff-docs` skill for full parameter details and workflows:

| Tool | Purpose |
|---|---|
| `handoff_doc_save` | Create/update a document — auto-splits `body` into fragments at `##` (configurable via `split_level`) |
| `handoff_doc_get` | Read a document — `full` (body), `meta` (metadata only), or `section` (one section by `seq`) |
| `handoff_doc_list` | List/search documents (BM25), filter by `doc_type`/`tags`/`task_id` |
| `handoff_doc_delete` | Delete a document and its fragments (also unlinks it from tasks) |
| `handoff_doc_reassemble` | Reconstruct the original Markdown from fragments, with drift detection |
| `handoff_doc_export` | Export documents as PDF (Typst is built into release binaries; a plain `cargo build` needs the Typst CLI; `doc_id`/`doc_ids`, `output_dir`, `template`, `include_toc`); `handoff_report` accepts `format: "pdf"` |
| `handoff_doc_tree` | Traverse the family tree — parent/child (structural) + related (semantic: `supersedes`/`references`/`implements`/`extends`/`conflicts`) |
| `handoff_doc_query` | Context injection (hook-driven) — staged `full`/`outline` results by fragment size |
| `handoff_doc_analyze` | Read-only heuristic scan of a file/directory — step 1 of importing existing docs |
| `handoff_doc_import` | Atomic bulk write of analyzed + AI-reviewed documents — step 3 of importing existing docs |
| `handoff_task_checklist` | Combined readiness view for a task — aggregates `done_criteria` with the verification matrices of its linked documents (`action="view"`, the only supported action, read-only). To generate verification items from acceptance criteria, use `handoff_trace_scaffold` |

`handoff_doc_save(task_ids: [...])` creates a **bidirectional** doc↔task
link: the document gains a `task_ids` entry and each linked task gains a
`TaskLink` in its `task_links`. Look it up from either side with
`handoff_doc_list(task_id: ...)` (docs linked to a task) or
`handoff_get_task(id: ...)` (inspect `task_links` on the task record).
Note: this is a **document-level** link. To link tasks to individual
requirements (SubItems), use `requirement_ids` on `handoff_update_task` —
see the `handoff-docs` skill for the distinction. For V-model layer documents
(requirements → specs → tests) see the `handoff-trace` skill; to move
`doc_req_import` requirements into one, see its `references/migration.md`.

### Configuration Management

`handoff_get_config()` returns the whole `config.toml` as JSON.
`handoff_update_config(updates: {"<dotted.key>": value, ...})` writes keys
(up to 4 segments; arrays and objects are accepted):
- **V-model**: `trace.profile`, `trace.done_guard`, `trace.auto_layer`,
  `trace.id_prefixes.<layer>` — every key is listed in the `handoff-trace`
  skill's `references/config.md`. `trace.*` values are not validated on
  write; check with `handoff_trace_report()` afterwards.
- **Calendar**: `calendar.work_hours_per_day`, `calendar.closed_weekdays`, `calendar.closed_dates`, `calendar.open_dates`, `calendar.schedule_mode`, `calendar.overwork_limit_percent`
- **Per-weekday hours**: `calendar.day_hours.fri` (number)
- **Budget**: `effort_budget.total_hours`
- **Assignees**: `assignees.<key>.display_name`, `assignees.<key>.color`, `assignees.<key>.work_hours_per_day`, `assignees.<key>.closed_weekdays`
- **Gantt view**: `gantt_view.sort`, `gantt_view.zoom`, `gantt_view.mode`, `gantt_view.group_by_milestone`, `gantt_view.show_workload`

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
