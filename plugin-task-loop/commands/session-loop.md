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
  |   |     → FAIL → rework notes → back to Stage 1
  |   |
  |   |   Stage 3 — REVIEW (full only):
  |   |     Single reviewer (Opus x1)
  |   |       - Design, test quality, spec coherence
  |   |     → APPROVE → done
  |   |     → REQUEST_CHANGES → rework notes → back to Stage 1
  |   |
  |-- Process results -> mark tasks done -> commit
  +-- Session handoff -> next session
```

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

| Parameter                  | Default  | Description                                                |
| -------------------------- | -------- | ---------------------------------------------------------- |
| `DEV_MODEL`                | `sonnet` | Model for developers                                       |
| `INTEGRATION_TESTER_MODEL` | `sonnet` | Model for the tester                                       |
| `REVIEWER_MODEL`           | `opus`   | Model for reviewer                                         |
| `MAX_TASKS_PER_SESSION`    | `5`      | Max tasks per session                                      |
| `MAX_ROUNDS`               | `3`      | Max main-loop rounds (implement → test → review = 1 round) |
| `integration_expected`     | `true`   | Must the session's work be wired into the system? (see 2c) |

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

1. Review task spec (`handoff_get_task` + spec documents). Also check
   `handoff_doc_query` for structured project documents (specs, designs, ADRs)
   relevant to the task's files — this can surface a spec the task description
   itself doesn't quote.
2. **Check readiness baseline**: `handoff_task_checklist(task_id=..., action="view")`
   — shows linked spec coverage and blockers upfront. If the task has a linked
   spec with a verification matrix, include its uncovered sections in the
   developer's instructions so they know exactly what to implement.
3. Draft implementation plan
4. **Identify uncertainties**:
   - Any ambiguous spec points?
   - Any decisions that need user input?
   - Any cross-session implications?
5. **Batch all uncertainties and confirm with the user** (goal: zero questions during implementation)
6. Start execution only after user approval

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

Notes on the cost:

- **`full` adds one serial turn over `standard`** for the reviewer (Opus). Pick `full` when
  the work is architecturally significant or cross-cutting.
- **`standard` is 2 serial turns**: implement (parallel developers) + test (single tester).
- **`express` has no tester or reviewer** — its definition ("mechanical and self-verifying")
  means there is no wiring to check. The developer is responsible for the whole-project
  suite, the build, and confirming its code is reachable.
- The developer runs format, lint, and type check under **every** profile, and the tests in
  its own scope. `express` drops the *adversarial* layers, not the gates.

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
it cannot be a per-task flag: with a mix of wired and unwired tasks the integration tester
cannot tell an intentional gap from a defect. Only you know which it is.

> A non-boolean value throws. `'false'` is a truthy string and would silently switch the check
> back **on** for a session that meant to suspend it.

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
- They touch the same file, module, or directory — one developer would have
  opened the same files twice anyway.
- Neither depends on the other's output (a dependency means they must be ordered,
  and a single agent doing both in sequence is fine — but say so in `instructions`).

Do **not** bundle across functional areas: a bundled agent that has to hold two
unrelated designs in context reasons worse than two focused agents, and one
failure drags the other into rework.

> **Bundled task IDs are opaque strings.** `t1+t2` is one ID end-to-end, matched
> whole (`t1` never collides with `t12`), and rework notes route back to it
> correctly. Use the exact same string in `tasks[].id` and `dev_assignments[].tasks`.
> Report and close the underlying tasks (`t1`, `t2`) individually in step 6.

### 4. (No tester assignments needed)

A single tester runs automatically for the entire session scope. There are no
`test_assignments` to write — the workflow reads all developer reports and feeds
them to one tester agent that covers both per-task adversarial verification and
whole-project integration testing.

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
    // 'express'  = dev only                              (1 serial turn)
    // 'standard' = dev -> tester                         (2 serial turns)
    // 'full'     = dev -> tester -> reviewer             (3 serial turns)
    // Omitted => 'standard'. An unknown value throws rather than downgrading.
    profile: 'standard',

    // --- Wiring expectation (see step 2c). Omitted => true. ---
    integration_expected: true,

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

    // --- Model defaults (per-assignment model_override takes priority) ---
    dev_model: 'sonnet',
    integration_tester_model: 'sonnet',
    reviewer_model: 'opus',

    // --- Loop control ---
    max_rounds: 3,  // max main-loop rounds (implement → test → review = 1 round)

    // --- Session context: fetched ONCE here, injected into every agent ---
    context: {
      branch: 'feat/xxx',
      prev_session_summary: 'Previous session summary',
      design_decisions: 'Design decisions',
      handoff_context: {
        decisions: [{ decision: '...', reason: '...', confidence: 'confirmed' }],
        handoff_notes: [{ category: 'caution', note: '...' }],
        next_actions: ['...'],
        memories: [{ title: '...', content: '...' }],
      },
    },
  },
});
```

> **Fetch once, inject many.** You already called `handoff_load_context` in step 0.
> Pass that result through as `context.handoff_context` instead of letting each
> developer, tester, and reviewer call it again — the answer is identical for all
> of them, and each call costs a ToolSearch plus an MCP round-trip.
>
> **Reasoning effort is set by the workflow, not by you.** It follows the profile:
> the `express` developer runs at `medium`, everyone else at `high`.

### 6. Process results and close tasks

The workflow returns:

| Field | Shape | Notes |
|---|---|---|
| `session_id` | string | echoed back from `args` |
| `profile` | string | the resolved profile (`express` / `standard` / `full`) |
| `stages_run` | object | `{ implement, test, integrate, review }` — which stages actually ran |
| `integration_expected` | boolean | the wiring expectation this session ran under |
| `passed` | boolean | every stage that ran concluded successfully |
| `rounds` | number | main-loop rounds actually run (always 1 for `express`) |
| `review_rework_rounds` | number | always 0 (kept for backward compat) |
| `task_ids` | string[] | the IDs you passed in |
| `dev_reports` | (string \| null)[] | `null` = that developer agent crashed |
| `test_reports` | any[] | always `[]` (kept for backward compat; scoped testers removed) |
| `integration_report` | object \| null | **structured**: `{ verdict, findings[], report }`. `null` under `express` or if it crashed |
| `review_report` | object \| null | **structured**: `{ verdict, findings[], report }`. `null` unless `full` ran |
| `review_escalation` | object \| null | present only after max rework rounds; `failed_stages` names which agent objected |
| `session_log` | object[] | per-round trace: one entry per `implement` / `test` / `review` stage, with verdicts and truncated summaries |

> **`passed: true` means less under a shallower profile.** Under `express` it
> means the developer's own gates passed — no independent verification ran, and
> **nothing checked that the code is wired into anything.** Read `stages_run`
> before treating a pass as verified.

> **`passed` is fail-closed across every layer that ran.** The tester and (under
> `full`) the reviewer must *both* pass. Either failing or crashing sends the
> session to rework. Read `integration_report.verdict` and `review_report.verdict`
> separately.

> **Verdicts are structured, not scraped.** The tester and the reviewer are called
> with a `schema`, so `.verdict` is an enum value — never parse prose to decide
> pass/fail. Read the human-readable markdown from the `.report` field.
>
> **A crashed agent (`null`) is treated as a failure, never as a pass.**

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

**On failure with escalation (passed: false, review_escalation present):**

The main loop did not pass after `max_rounds`. Read `review_escalation.failed_stages` — it
names which agent objected (test, review, or both).

Under `full`, the reviewer on the last round was told it is the escalation round and has
written escalation context to handoff. Under `standard` **no reviewer ran, so nothing was
written** — `review_escalation.reason` says so, and surfacing it is on you.

The manager should:

1. Leave tasks in `review` status
2. Record the escalation summary in `notes_append`, including which stage failed
3. Report to the user with the unresolved issues from `review_escalation.final_review`
   and/or `review_escalation.final_integration`
4. Close session (step 7) with `caution` handoff notes referencing the escalation
5. Under `full`, the next session's step 0 picks up the reviewer's escalation automatically

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
