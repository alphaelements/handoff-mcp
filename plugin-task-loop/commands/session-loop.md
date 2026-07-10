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
  |-- Plan implementation + clarify uncertainties upfront
  |-- Workflow(session-execute)
  |   |-- Inner loop (up to 3 rounds):
  |   |   |-- Phase 1: Parallel developers (Sonnet xN)
  |   |   +-- Phase 2: Parallel testers (Sonnet xN)
  |   |   (test FAIL -> rework, repeat inner loop)
  |   |
  |   |-- Final Review (1x after tests pass):
  |   |   +-- Reviewer (Opus x1)
  |   |   APPROVE -> done
  |   |   REQUEST_CHANGES -> Review rework loop (max 2 rounds):
  |   |     |-- Implement rework
  |   |     |-- Test rework
  |   |     +-- Re-review
  |   |     (Still REQUEST_CHANGES after max rounds -> escalate to handoff)
  |   |
  |-- Process results -> mark tasks done -> commit
  +-- Session handoff -> next session
```

## Configuration parameters

| Parameter               | Default  | Description                                                |
| ----------------------- | -------- | ---------------------------------------------------------- |
| `DEV_MODEL`             | `sonnet` | Model for developers                                       |
| `TESTER_MODEL`          | `sonnet` | Model for testers                                          |
| `REVIEWER_MODEL`        | `opus`   | Model for reviewer                                         |
| `MAX_TASKS_PER_SESSION` | `5`      | Max tasks per session                                      |
| `MAX_REWORK_ROUNDS`     | `3`      | Max test-level rework rounds                               |
| `MAX_REVIEW_ROUNDS`     | `2`      | Max review rework rounds after final review                |

These can be adjusted via prompt arguments. Future versions may read from `handoff_get_config`.

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

1. Review task spec (`handoff_get_task` + spec documents)
2. Draft implementation plan
3. **Identify uncertainties**:
   - Any ambiguous spec points?
   - Any decisions that need user input?
   - Any cross-session implications?
4. **Batch all uncertainties and confirm with the user** (goal: zero questions during implementation)
5. Start execution only after user approval

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

The developer runs the project's quality gates (format, lint, type check, test)
under **every** profile. `express` drops the *adversarial* layers, not the gates.

**Present the chosen profile to the user together with the session plan in step 2**,
state which rule selected it, and let the user override it. Record the final
choice in the session notes.

### 3. Assign developers

- Assign tasks so **file scopes don't conflict** between developers
- Default model for all developers is Sonnet. Use `model_override` in dev_assignments
  only when explicitly requested by the user.
- 1-2 tasks per developer (small tasks can be bundled)

> **Bundled task IDs** (`t1+t2`) are supported: the ID is treated as one opaque
> string end-to-end, and rework notes route back to it correctly. Use the exact
> same string in `tasks[].id`, `dev_assignments[].tasks`, and
> `test_assignments[].task_ids`. IDs are matched whole — `t1` never collides with
> `t12`.

### 4. Assign testers

- Distribute so **testing workload is roughly equal** across testers
- Bundle quick checks (lint, type check) with one tester
- Spread time-consuming work (integration tests, E2E) across testers
- Aim for similar completion time across all testers

### 5. Launch Workflow

**Always use `name: "handoff-task-loop:session-execute"` to invoke the predefined workflow. Never write an inline script.**
The predefined workflow correctly routes `agentType` and `model` settings.
Inline scripts would bypass agent definitions (session-developer = Sonnet, session-reviewer = Opus, etc.).
**All customization goes through `args`.** This gives full control over team size, models, instructions,
and verification scope.

> **Note:** The Workflow runtime may pass `args` as a JSON string rather than an object.
> `session-execute.js` handles this internally. If writing custom workflow scripts,
> always add a parse guard at the top: `const _args = typeof args === 'string' ? JSON.parse(args) : (args || {});`

> **Resuming a Workflow run**: `resumeFromRunId` does NOT auto-inherit
> `args` from the previous run — it is part of the cache key. Always
> pass the same `args` object again explicitly when resuming:
> `Workflow({ scriptPath, resumeFromRunId, args: { ...same args... } })`.
> Omitting `args` on resume causes an early validation error (see below).

```javascript
Workflow({
  name: 'handoff-task-loop:session-execute',
  args: {
    session_id: '<id>',

    // --- Pipeline depth (see step 2b) ---
    // 'express' (dev only) | 'standard' (dev -> test) | 'full' (dev -> test -> review)
    // Omitted => 'standard'. An unknown value throws rather than downgrading.
    // 'express' does not take test_assignments.
    profile: 'standard',

    // --- Task definitions (instructions field for detailed guidance) ---
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

    // --- Developer assignments ---
    dev_assignments: [
      { dev_label: 'A', tasks: ['t1+t2'] },
      { dev_label: 'B', tasks: ['t3'] },
    ],

    // --- Tester assignments (flexible team size and instructions) ---
    test_assignments: [
      {
        tester_label: 'A',
        task_ids: ['t1+t2'],
        instructions: 'Test valid/invalid inputs, boundary values, and error response format',
      },
      {
        tester_label: 'B',
        task_ids: ['t3'],
        instructions: 'Concurrent request stress test, window boundary edge cases',
      },
    ],

    // --- Model defaults (per-assignment model_override takes priority) ---
    dev_model: 'sonnet',
    tester_model: 'sonnet',
    reviewer_model: 'opus',

    // --- Loop control ---
    max_rounds: 3,
    max_review_rounds: 2,

    // --- Session context ---
    context: {
      branch: 'feat/xxx',
      prev_session_summary: 'Previous session summary',
      design_decisions: 'Design decisions',
    },
  },
});
```

### 6. Process results and close tasks

The workflow returns:

| Field | Shape | Notes |
|---|---|---|
| `profile` | string | the resolved profile (`express` / `standard` / `full`) |
| `stages_run` | object | `{ implement, test, review }` — which stages actually ran |
| `passed` | boolean | every stage that ran concluded successfully |
| `rounds` | number | inner test-loop rounds actually run (always 1 for `express`) |
| `review_rework_rounds` | number | review-rework rounds actually run (0 unless `full`) |
| `task_ids` | string[] | the IDs you passed in |
| `dev_reports` | (string \| null)[] | `null` = that developer agent crashed |
| `test_reports` | (object \| null)[] | **structured**: `{ verdict, tasks[], report }`. `null` = crashed. `[]` under `express` |
| `review_report` | object \| null | **structured**: `{ verdict, findings[], report }`. `null` unless `full` ran |
| `review_escalation` | object \| null | present only after max review-rework rounds |

> **`passed: true` means less under a shallower profile.** Under `express` it
> means the developer's own gates passed — no independent verification ran.
> Read `stages_run` before treating a pass as reviewed.

> **Verdicts are structured, not scraped.** Testers and the reviewer are called with
> a `schema`, so `test_reports[i].verdict` and `review_report.verdict` are enum
> values — never parse prose to decide pass/fail. Read the human-readable markdown
> from the `.report` field.
>
> **A crashed agent (`null`) is treated as a failure, never as a pass.**
> `parallel()` resolves a thrown thunk to `null`, so fail-closed is the only safe
> reading.

After receiving the Workflow result:

**On success (passed: true):**

1. Check off each task's done_criteria:
   ```
   handoff_check_criterion(task_id, criterion_index, checked=true)
   ```
2. Mark tasks as done:
   ```
   handoff_update_task(task={ id, status: "done",
     notes_append: "## session-loop result\n<summary>" })
   ```
3. Create report tasks for discovered issues (`_bug-report-protocol.md`)
4. Commit:
   ```bash
   # Run the project's quality gates from CLAUDE.md (format, type check, test, lint)
   # Then: git add <changed files> && git commit
   ```
5. Log to session state file

**On failure (passed: false, tests never passed):**

- Leave tasks in `review` status
- Record failure reason and feedback in `notes_append`
- Report to user and ask for guidance
- **Still close the session (step 7) regardless**

**On failure with review escalation (passed: false, review_escalation present):**

The reviewer has already written escalation context to handoff (via `handoff_save_context`
and `handoff_memory_save`). The manager should:

1. Leave tasks in `review` status
2. Record the escalation summary in `notes_append`
3. Report to user with the specific unresolved issues from `review_escalation.final_review`
4. Close session (step 7) with `caution` handoff notes referencing the escalation
5. The next session's step 0 will pick up the escalation context automatically

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

| Format         | Meaning                           | Example                  |
| -------------- | --------------------------------- | ------------------------ |
| `t1,t2,t3`    | Specific IDs only (comma-sep)     | `/session-loop t1,t2`    |
| `t5-`          | All todo from t5 onward           | `/session-loop t5-`      |
| `t5-t9`        | Range (inclusive)                  | `/session-loop t5-t9`    |
| `goal: <cond>` | Natural language stop condition   | `/session-loop goal: ...`|
| (no args)      | All todo tasks                    | `/session-loop`          |

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
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only.
- **Do not push.** Stop at commit.
- **Always use `name: "handoff-task-loop:session-execute"` for the Workflow.** Never write inline scripts.
  Inline scripts bypass agent definitions (agentType routing) and model settings.
  All customization goes through `args`.
