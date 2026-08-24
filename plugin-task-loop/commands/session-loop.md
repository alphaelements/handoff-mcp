---
description: Automated task consumption loop — parallel TDD implementation, testing, and review per session (session manager procedure)
argument-hint: '[task selector] e.g. t1,t2,t3 | t5- | t5-t9 | goal: condition'
---

# Session-based Task Loop (Session Manager)

You are the **session manager**. You do not implement, test, or review code yourself.
Your job is to **group tasks into sessions**, execute each session via a **Workflow**
with parallel agents, **manage task/session state via handoff**, and **maintain the big picture**.

## Flow overview

```
Fetch all tasks -> Split into sessions -> User approval
  |
Session N:
  |-- Plan implementation (incl. doc lookup) + clarify uncertainties upfront
  |-- Mark session's tasks in_progress
  |-- Workflow(session-execute)
  |   |-- Main loop (up to 3 rounds, rework restarts at Stage 1):
  |   |
  |   |   Stage 1 — IMPLEMENT:
  |   |     All developers run in parallel (Sonnet)
  |   |     → All reported → Stage 2
  |   |     → Any crashed  → session failed (break)
  |   |
  |   |   Stage 2 — TEST (standard, full):
  |   |     Single tester (Sonnet x1)
  |   |       - Per-task adversarial verification (mutation, old-code, fallback)
  |   |       - Whole-project quality gates, E2E, wiring
  |   |     → PASS → Stage 3
  |   |     → FAIL, round < max     → rework notes → back to Stage 1
  |   |     → FAIL, round == max    → done, findings become follow-up tasks (no escalation)
  |   |
  |   |   Stage 3 — REVIEW (full only):
  |   |     Single reviewer (Opus x1)
  |   |       - Design, test quality, spec coherence
  |   |     → APPROVE → done
  |   |     → REQUEST_CHANGES, round < max  → rework notes → back to Stage 1
  |   |     → REQUEST_CHANGES, round == max → done, findings become follow-up tasks (no escalation)
  |   |
  |-- Process results -> check off done_criteria -> mark tasks done -> file
  |   discovered issues + pending_followups as tasks -> record durable
  |   findings as docs -> commit
  +-- Session handoff -> next session
```

> **Multi-WT mode (opt-in, off by default).** When `config.toml`
> `[worktree.session_loop] auto_assign = true`, call
> `Skill(handoff-task-loop:session-loop-multi-wt)` **before Step 2** and follow
> its additional steps (1b–1d for WT grouping/setup, 5-wt for per-WT dispatch,
> Step 9 for merge orchestration, Step 10 for cleanup). When `auto_assign = false`
> (the default) or absent, ignore multi-WT entirely — the flow above runs unchanged.

## The three verification layers

| Layer | Agent | Scope | Answers |
|---|---|---|---|
| developer | Sonnet (parallel) | its own tasks | does my change work? (red → green, quality gates) |
| tester | Sonnet (1 agent) | the whole session | **what do the tests fail to guarantee?** + is it wired? + does the whole suite pass? |
| reviewer (`full`) | Opus (1 agent) | everything, incl. test code | is the design right? is the test code itself correct? |

Key properties:

- **One tester covers everything.** It does both per-task adversarial verification (mutation
  checks, old-code checks, fallback audits) AND whole-project integration testing (quality
  gates, E2E, wiring). There is no separate scoped tester.
- **Stages are strictly serial.** Testing starts only after ALL developers finish. Review
  starts only after testing passes. This eliminates the old nested-loop complexity.

## Configuration parameters

| Parameter | Default | Description |
|---|---|---|
| `dev_model` | `sonnet` | Model for developers (per-assignment `model_override` available) |
| `integration_tester_model` | `sonnet` | Model for the tester |
| `reviewer_model` | `opus` | Model for reviewer |
| `MAX_TASKS_PER_SESSION` | `5` | Max tasks per session |
| `max_rounds` | `3` | Max main-loop rounds (implement → test → review = 1 round) |
| `integration_expected` | `true` | Must the session's work be wired into the system? (see 2c) |

### Budget configuration

Per-role budgets are **advisory** — injected into agent prompts, not enforced at runtime.

| Role | Default max_turns | Default max_tool_calls | Default soft_wall_time_s |
|---|---|---|---|
| `developer` | 80 | 200 | 900 (15 min) |
| `integration-tester` | 60 | 150 | 600 (10 min) |
| `reviewer` | 40 | 100 | 600 (10 min) |

Pass `budgets` arg to opt in. Omit for no budget section. `budgets: {}` opts in with all defaults.
Partial overrides merge with defaults. At 90% utilization agents report progress; at 100% the
coordinator decides whether to continue, split, or stop.

## Detailed procedure

### 0. Establish session (MUST run at the start of every session)

**Not just the first time — every session start.** Load the handoff from the previous
session's close (step 7) and establish a new active session.
Skipping this breaks the handoff chain.

```
handoff_load_context
-> Review previous session's decisions / context_pointers / next_actions / handoff_notes
-> If no active session (= previous session was properly closed):
  handoff_save_context(
    session_status="active",
    summary="Session N: <target tasks summary>",
    related_task_ids=[...],
    label="Session N: <brief description>")
-> Read suggestion notes and continue from where the previous session left off
```

### 1. Fetch tasks and split into sessions

```
handoff_list_tasks(status_filter="todo")
```

- Fetch all todo tasks
- Analyze dependencies, priorities, and complexity
- **1 session = 1-5 tasks** (adjust based on scale)
  - Group tasks in the same functional area (avoid file conflicts between developers)
  - Tasks with dependencies go to earlier sessions
- **Present the full session plan to the user for approval**

### 2. Plan session implementation

For each task in the session:

1. Review task spec (`handoff_get_task` + spec documents). Also check
   `handoff_doc_query` for structured project documents (specs, designs, ADRs)
   relevant to the task's files — this can surface a spec the task description
   itself doesn't quote. **Do this even though the developer agent will also call
   `handoff_doc_query` itself** — you need the result now to write the task's
   `instructions`, and a doc you found but didn't mention is a doc the developer
   has to rediscover from scratch.
2. **Check readiness baseline**: `handoff_task_checklist(task_id=..., action="view")`
   — shows linked spec coverage and blockers upfront. If the task has a linked
   spec with a verification matrix, include its uncovered sections in the
   developer's instructions so they know exactly what to implement.
3. **Fold every doc found in step 1 into the task's `instructions` field** —
   name the doc path and the sections relevant to this task. Don't rely on the
   developer's own `handoff_doc_query` call to be the only time this spec surfaces.
4. Draft implementation plan
5. **Identify uncertainties**:
   - Any ambiguous spec points?
   - Any decisions that need user input?
   - Any cross-session implications?
6. **Batch all uncertainties and confirm with the user** (goal: zero questions during implementation)
7. Start execution only after user approval

### 2b. Choose the pipeline profile

The profile decides how many **serial agent turns** the session costs — the
dominant term in wall-clock latency. Pick it mechanically from the tasks in the
session, then let the user override it.

| Profile | Stages | Serial turns | Use when |
|---|---|---|---|
| `express` | developer | 1 | Every task is mechanical and self-verifying |
| `standard` | developer → tester | 2 | **Default.** Ordinary feature or bug work |
| `full` | developer → tester → reviewer | 3 | Architecture, cross-cutting, or risky change |

Apply the first rule that matches, evaluated over **all** tasks in the session:

1. **`full`** if any task carries the label `architecture` or `refactor`, **or**
   any task's `schedule.estimate_hours` is `> 4`, **or** the session spans more
   than one functional area (developers touching unrelated directories).
2. **`express`** if *every* task has `estimate_hours <= 1`, carries none of the
   labels above, and is confined to a single file or a mechanical edit
   (rename, version bump, doc fix, adding a test to an existing suite).
3. **`standard`** otherwise.

Two rules that override the table:

- **A task labelled `bug` never uses `express`.** A bug fix needs an adversarial
  check that the bug is actually gone, and the developer who wrote the fix is
  the worst person to make that call.
- **Escalate on rework.** If a session fails and you re-run it, raise the profile
  one level (`express` → `standard` → `full`). Repeating a failed run at the same
  depth just spends tokens to reach the same conclusion.

**Present the chosen profile to the user together with the session plan in step 2**,
state which rule selected it, and let the user override it. Record the final
choice in the session notes.

### 2c. Decide `integration_expected`

Does the code this session writes have to be **wired into the system** by the time the
session ends?

- **`true` (default)** — the session's work must be reachable from a real entry point (a CLI
  command, a tool dispatch, a route, a registered handler). The integration tester **FAILs**
  implemented-but-unconnected code, even when every unit test is green.
- **`false`** — this session deliberately builds a foundation and wires it in a later session.
  Unwired code is recorded under `### Wiring status`, not failed. **The whole-project suite
  and E2E still run and must still pass**; only the wiring verdict is suspended.

Set `false` only when you planned it that way. It is a property of **the session's scope**, so
it cannot be a per-task flag.

### 3. Assign developers

- Assign tasks so **file scopes don't conflict** between developers
- Default model for all developers is Sonnet. Use `model_override` in dev_assignments
  only when explicitly requested by the user.
- 1-2 tasks per developer

#### Bundle small tasks aggressively

Every agent you launch costs a fixed overhead — spawn, context load, its own
`handoff_get_task` and `handoff_memory_query` round-trips — before it writes a
single line. For a 15-minute task that overhead is noise; for a 3-minute task it
is most of the bill.

**Bundle tasks into one agent when they are small and touch the same code.**
Join their IDs with `+`:

```javascript
tasks: [{ id: 't1+t2', title: 'Validate input and normalize error responses', ... }],
dev_assignments: [{ dev_label: 'A', tasks: ['t1+t2'] }],
```

Bundle when **all** of these hold:

- Each task is `estimate_hours <= 1`.
- They touch the same file, module, or directory.
- Neither depends on the other's output (a dependency means they must be ordered,
  and a single agent doing both in sequence is fine — but say so in `instructions`).

Do **not** bundle across functional areas.

> **Bundled task IDs are opaque strings.** `t1+t2` is one ID end-to-end, matched
> whole (`t1` never collides with `t12`), and rework notes route back to it
> correctly. Use the exact same string in `tasks[].id` and `dev_assignments[].tasks`.
> Report and close the underlying tasks (`t1`, `t2`) individually in step 6.

### 4. (No tester assignments needed)

A single tester runs automatically for the entire session scope. There are no
`test_assignments` to write.

### 4b. Mark tasks in_progress

Before launching the Workflow, update every task in this session's scope to `in_progress`:

```
handoff_update_task(task={ id, status: "in_progress" })  // once per task ID
```

Do this **outside** the Workflow, not inside it — concurrent writes from parallel
developers to the same task record is exactly the failure mode this avoids.
For a bundled `t1+t2`, update `t1` and `t2` individually.

### 5. Launch Workflow

**Always use `name: "handoff-task-loop:session-execute"` to invoke the predefined workflow. Never write an inline script.**
Inline scripts bypass agent definitions (session-developer = Sonnet, session-reviewer = Opus, etc.).
**All customization goes through `args`.**

> **Resuming a Workflow run**: `resumeFromRunId` does NOT auto-inherit `args`.
> Always pass the same `args` object again explicitly when resuming.

```javascript
Workflow({
  name: 'handoff-task-loop:session-execute',
  args: {
    session_id: '<id>',
    profile: 'standard',        // express | standard | full
    integration_expected: true,  // see step 2c
    tasks: [
      {
        id: 't1+t2',
        title: 'Add input validation to API endpoint',
        done_criteria: ['All inputs validated', 'Error responses follow RFC 7807'],
        instructions: 'Add schema validation middleware using the existing validator pattern...',
      },
      {
        id: 't3',
        title: 'Implement rate limiting',
        done_criteria: ['Rate limiter active', 'Returns 429 with Retry-After header'],
        instructions: 'Use sliding window algorithm with configurable limits...',
      },
    ],
    dev_assignments: [
      { dev_label: 'A', tasks: ['t1+t2'] },
      { dev_label: 'B', tasks: ['t3'] },
    ],
    dev_model: 'sonnet',
    integration_tester_model: 'sonnet',
    reviewer_model: 'opus',
    max_rounds: 3,
    // Optional: budgets: { developer: { max_turns: 80 }, ... }
    context: {
      branch: 'feat/xxx',
      prev_session_summary: '...',
      design_decisions: '...',
      handoff_context: { /* from handoff_load_context step 0 */ },
    },
  },
});
```

> **Fetch once, inject many.** Pass `handoff_load_context` result as `context.handoff_context`
> instead of letting each agent call it again.
>
> **Reasoning effort is set by the workflow, not by you.** `express` developer = `medium`, rest = `high`.

### 6. Process results and close tasks

The workflow returns:

| Field | Shape | Notes |
|---|---|---|
| `passed` | boolean | `true` = converged or `max_rounds` reached (remainder → `pending_followups`). `false` = developer crashed with no report |
| `profile` | string | resolved profile |
| `stages_run` | object | `{ implement, test, integrate, review }` |
| `dev_reports` | (string\|null)[] | `null` = developer crashed |
| `integration_report` | object\|null | `{ verdict, findings[], report }`. `null` under `express` or crash |
| `review_report` | object\|null | `{ verdict, findings[], report }`. `null` unless `full` ran |
| `pending_followups` | object[] | `{ source, task_id, severity, location, problem, crashed }`. Empty when converged |
| `session_log` | object[] | per-round trace |

> **`passed: true` means less under a shallower profile.** Under `express` it means the
> developer's own gates passed — no independent verification ran. Read `stages_run`.

> **Always check `pending_followups` even when `passed: true`.** The workflow demotes
> unresolved findings to followups rather than failing the session. A `crashed: true` entry
> means the agent produced no report (BLOCKER) — absence of review, not absence of defects.

> **Verdicts are structured, not scraped.** `.verdict` is an enum. Read human-readable
> content from `.report`. A crashed agent (`null`) is a failure, never a pass.

#### Check off done_criteria — every round, regardless of pass/fail

Read `dev_reports` for `### done_criteria progress` lines. For every `met: true`, call:

```
handoff_check_criterion(task_id, criterion_index, checked=true)
```

Do this **before** branching on `passed`. Partial progress should not wait for a session-wide pass.

#### On success (passed: true, pending_followups empty)

1. (done_criteria already checked off above)
2. Mark tasks done: `handoff_update_task(task={ id, status: "done", notes_append: "..." })`
3. **File discovered issues** (follow `_bug-report-protocol.md`):
   - Collect `### Discovered issues` from `dev_reports`, `integration_report`, `review_report`
   - Duplicate-check via `handoff_list_tasks`
   - Create via `handoff_update_task` (omit `id`): title `[bug]`/`[improvement]`/`[spec]`,
     `labels: ["found-during-loop", "<type>"]`, priority matching severity
4. Persist durable findings via `handoff_doc_save` / `handoff_memory_save` if warranted
5. Run quality gates, then commit

#### On success with pending_followups (passed: true, non-empty)

Same as above, plus **before commit**:

1. Duplicate-check each followup against `handoff_list_tasks`
2. Create follow-up tasks via `handoff_update_task` (omit `id`):
   - Title: `[review-followup]` prefix (add `[agent-crashed]` if `crashed: true`)
   - Status: `todo` (backlog)
   - Priority: `high` for BLOCKER / `crashed: true`, `medium` for MAJOR, `low` for MINOR/NIT
   - Labels: `["found-during-loop", "review-followup"]` (add `"agent-crash"` when `crashed: true`)
   - Notes: problem, location, source stage, originating task_id, session ID
3. Write a doc via `handoff_doc_save` for the batch — what's unresolved, recommended fix, open questions
4. Tell the user explicitly — this is not a silent pass. If any `crashed: true`, call it out.
5. Mark original tasks `done` — followups are separate backlog items

#### On failure (passed: false, no developer reported)

- Leave tasks in `review` status
- Record failure reason in `notes_append`
- **Still file discovered issues and close the session (step 7)**
- Report to user and ask for guidance

### 7. Close session and handoff (MUST run at every session end)

**Regardless of step 6 success/failure, always close the session.**
Skipping this breaks the handoff chain for the next session.

```
handoff_save_context(
  session_status="closed",
  summary="Session N complete: <summary of what was done>",
  decisions=[
    { decision: "<what was decided>", confidence: "confirmed", reason: "<why>" }
  ],
  handoff_notes=[
    { category: "suggestion",
      note: "Done: <what was implemented/fixed>. Next: <what the next session should do>" },
    { category: "caution", note: "<risks or caveats for the next session>" },
    { category: "context", note: "<background the next session needs>" }
  ],
  context_pointers=[
    { path: "<file the next session should read>", reason: "<why>" }
  ],
  related_task_ids=["<completed task IDs>"]
)
```

**handoff_notes must include:**

- `suggestion` (required): What's done + next action. Read at step 0 of the next session.
- `caution`: Caveats (unresolved issues, failed tasks, known constraints)
- `context`: Background the next session needs (design decision rationale, etc.)

### 8. Next session

- If the goal is not yet met, `/loop` triggers the next iteration
- **Step 0 runs at the top of each iteration**, loading the handoff from step 7
- This ensures "Session N completion -> Session N+1 start" is properly chained

## Task selector (argument parsing)

Users can scope the loop via arguments to `/session-loop`.
The manager parses these and filters `handoff_list_tasks` results accordingly.

| Format | Meaning | Example |
|---|---|---|
| `t1,t2,t3` | Specific IDs only (comma-sep) | `/session-loop t1,t2` |
| `t5-` | All todo from t5 onward | `/session-loop t5-` |
| `t5-t9` | Range (inclusive) | `/session-loop t5-t9` |
| `goal: <cond>` | Natural language stop condition | `/session-loop goal: ...` |
| (no args) | All todo tasks | `/session-loop` |

- Tasks with non-`todo` status are skipped (reported to user).
- Open-ended ranges (`t5-`) include all todo tasks with IDs >= t5.
- Mixed formats (`t1,t3-t5`) are supported.

## Goal (stop condition)

With task selector: Stop when all specified tasks are done.
Without args (default): **Stop when zero todo tasks remain in handoff.**

Each iteration checks `handoff_list_tasks(status_filter="todo")`.
If target tasks remain, continue. If zero, run completion procedure.

## Completion (when goal is met)

1. `handoff_save_context` with final summary
2. Report to user and end the loop

## Rules

- **Do not start implementation without user approval** (session plan + uncertainties first)
- **Never fake a completion report.** If the reviewer says FAIL, don't close the task.
- **Never swallow discovered issues.** Follow `_bug-report-protocol.md`.
- **Never leave a session's tasks sitting in `todo` while the Workflow runs.** Mark
  them `in_progress` in step 4b, before the Workflow call.
- **Check off done_criteria as reports come in, not only when the session passes.**
  A rework round can genuinely satisfy some criteria — don't wait for the whole
  session to pass to record that.
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only.
- **Do not push.** Stop at commit.
- **Always use `name: "handoff-task-loop:session-execute"` for the Workflow.** Never write inline scripts.
  Inline scripts bypass agent definitions (agentType routing) and model settings.
  All customization goes through `args`.
