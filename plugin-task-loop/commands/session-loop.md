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
  |   |   Each independent work group pipelines on its own:
  |   |     group 1: developer -> tester ----.
  |   |     group 2: developer -> tester ----+-> round barrier
  |   |     group 3: developer -> tester ----'
  |   |   (a group's tester starts as soon as ITS developers finish —
  |   |    it never waits on an unrelated group's developer)
  |   |   (each verifies ONLY its own scope; test FAIL -> rework, repeat)
  |   |
  |   |-- Verify stage (1x after the inner loop converges) — both run CONCURRENTLY:
  |   |   +-- Integration tester (Sonnet x1)  whole suite / E2E / wiring
  |   |   +-- Reviewer (Opus x1, `full` only) design / is the test code itself right?
  |   |   BOTH pass -> done
  |   |   EITHER fails -> Rework loop (max 2 rounds):
  |   |     |-- Implement + test rework (same per-group pipeline)
  |   |     +-- Re-verify (both agents again)
  |   |     (Still failing after max rounds -> escalate to handoff)
  |   |
  |-- Process results -> mark tasks done -> commit
  +-- Session handoff -> next session
```

> **Work groups.** A group is a connected component of
> `developer --owns--> task <--verifies-- tester`. Assign a tester the tasks of
> exactly one developer and you get one group per developer, all pipelining
> independently. Give one tester the tasks of two developers and those three
> agents fuse into a single group that waits internally — a real dependency, since
> that tester reads both developers' reports. Round convergence stays
> session-wide, so `rounds` keeps its meaning and the verify stage always sees a
> coherent snapshot.

## The four verification layers

Each layer is defined by **what only it can see**, not by who runs the test command.

| Layer | Scope | Timing | Answers |
|---|---|---|---|
| developer | its own tasks | in-group, parallel | does my change work? (red -> green) |
| tester | its own tasks | in-group, parallel | **what does this test suite fail to guarantee?** |
| integration tester | the whole tree | once, after the round barrier | is it wired? does the whole suite and E2E pass? |
| reviewer (`full`) | everything, incl. test code | once, **alongside** the integrator | is the design right? is the test code itself correct? |

Two consequences worth internalizing:

- **The tester does not run the whole suite or E2E, and does not judge wiring.** It cannot:
  while it runs, another group may still be implementing, so any whole-tree verdict would be
  a verdict on a half-built tree. It also does not re-run what the developer already ran green
  — that yields no information. Its job is to attack the tests themselves: do they execute, do
  they go red when the implementation is broken, would they have passed against the old code,
  and what does no test cover?
- **Wiring is checked exactly once, at the end.** The defect this catches: every unit test is
  green, every tester said PASS, and the feature does not work because nothing calls it.

## Configuration parameters

| Parameter                  | Default  | Description                                                |
| -------------------------- | -------- | ---------------------------------------------------------- |
| `DEV_MODEL`                | `sonnet` | Model for developers                                       |
| `TESTER_MODEL`             | `sonnet` | Model for testers                                          |
| `INTEGRATION_TESTER_MODEL` | `sonnet` | Model for the integration tester                           |
| `REVIEWER_MODEL`           | `opus`   | Model for reviewer                                         |
| `MAX_TASKS_PER_SESSION`    | `5`      | Max tasks per session                                      |
| `MAX_REWORK_ROUNDS`        | `3`      | Max test-level rework rounds                               |
| `MAX_REVIEW_ROUNDS`        | `2`      | Max rework rounds after the verify stage                   |
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
| `standard` | developer → tester → integrate | 3 | **Default.** Ordinary feature or bug work |
| `full` | developer → tester → (integrate ∥ review) | 3 | Architecture, cross-cutting, or risky change |

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

- **`full` costs the same three serial turns as `standard`.** The integration tester and
  the reviewer run in one `parallel()` barrier, so `full` buys the reviewer for free in
  wall-clock terms. When in doubt between the two, `full` is cheap.
- **`express` has no integration tester** — its definition ("mechanical and self-verifying")
  means there is no wiring to check. There, and only there, the developer is responsible for
  the whole-project suite, the build, and confirming its code is reachable.
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
test_assignments: [{ tester_label: 'A', task_ids: ['t1+t2'] }],
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
> correctly. Use the exact same string in `tasks[].id`, `dev_assignments[].tasks`,
> and `test_assignments[].task_ids`. Report and close the underlying tasks
> (`t1`, `t2`) individually in step 6.

### 4. Assign testers

- Distribute so **testing workload is roughly equal** across testers
- Aim for similar completion time across all testers
- Use `instructions` to point a tester at the specific attack surface of its tasks

> **Testers are scoped, and there is nothing to assign for the integration stage.**
> A tester verifies only its own `task_ids`: it does not run the whole suite, does not
> run E2E, and does not judge wiring. Exactly one integration tester runs per session,
> over the whole tree, so there are no `integration_assignments` to write.

#### Keep testers inside one developer's task set

The implement and test stages **pipeline per work group**: a tester starts as
soon as the developers it depends on finish, not when the whole session's
developers finish. The shape of your assignment decides how much of that you get.

- **Prefer a 1:1 tester-to-developer mapping.** `test_assignments[i].task_ids`
  equal to `dev_assignments[i].tasks` gives one group per developer, and every
  group pipelines independently. A slow developer then delays only its own tester.
- **A tester spanning two developers fuses them into one group.** That tester
  reads both developers' reports, so it genuinely must wait for both — the
  workflow enforces it. It is sometimes the right call (a cross-cutting
  integration check), but it serializes the two developers behind the slower one.

A session of N developers with a 1:1 tester mapping costs
`max over groups of (developer + tester)`. The same session with one tester
covering everything costs `max(developer) + tester`. Choose deliberately.

> **Every task you implement must appear in some tester's `task_ids`.** The
> workflow does not enforce this: a task with no tester is implemented, never
> verified, and the session still reports `passed: true`. Under `standard` and
> `full` that silently buys you nothing for that task. Check the union of
> `test_assignments[].task_ids` against `tasks[].id` before launching.

> **Keep the group count at or below the runtime's concurrent-agent cap**
> (`min(16, cores - 2)`). A session is 1-5 tasks, so this holds by default. If a
> future session fans out wider than the cap, the pipeline can become *slower*
> than the old barrier: a fast group's tester takes a slot that a slow group's
> developer has not claimed yet, lengthening the critical path.

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
    // 'express'  = dev                                   (1 serial turn)
    // 'standard' = dev -> test -> integrate              (3 serial turns)
    // 'full'     = dev -> test -> (integrate ∥ review)   (3 serial turns)
    // Omitted => 'standard'. An unknown value throws rather than downgrading.
    // 'express' does not take test_assignments.
    profile: 'standard',

    // --- Wiring expectation (see step 2c). Omitted => true. ---
    // false only when this session deliberately leaves its work unwired for a
    // later session. The whole suite and E2E still run either way.
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
    integration_tester_model: 'sonnet',
    reviewer_model: 'opus',

    // --- Loop control ---
    max_rounds: 3,        // inner implement+test rounds
    max_review_rounds: 2, // rework rounds after the verify stage fails

    // --- Session context: fetched ONCE here, injected into every agent ---
    context: {
      branch: 'feat/xxx',
      prev_session_summary: 'Previous session summary',
      design_decisions: 'Design decisions',

      // Your own step-0 `handoff_load_context` result. Pass it through and no
      // agent pays a ToolSearch + MCP round-trip to read the same bytes.
      //
      // You may forward the tool's response VERBATIM: `decisions` and
      // `handoff_notes` nested under `previous_session` are read from there, and
      // keys the agents cannot use (`session_guidance`, `task_summary`, ...) are
      // ignored rather than dumped into the prompt. A flat object or a
      // pre-formatted string also work.
      handoff_context: {
        decisions: [{ decision: '...', reason: '...', confidence: 'confirmed' }],
        handoff_notes: [{ category: 'caution', note: '...' }],
        next_actions: ['...'],
        memories: [{ title: '...', content: '...' }], // optional: pre-fetched memories
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
> What agents still fetch themselves is what depends on **their own** work:
> `handoff_get_task` (the manager passes only title / done_criteria / instructions,
> so notes, labels, links, and dependencies would otherwise be lost),
> `handoff_memory_query` and `handoff_doc_query` (which memory/doc matters depends
> on the files touched — unlike `memories`, `handoff_context` has no `docs` field
> the runtime renders, so `handoff_doc_query` is always a per-agent call, not
> something the manager pre-fetches once), and — reviewer only —
> `handoff_list_tasks` (cross-task duplicate detection).
>
> **Reasoning effort is set by the workflow, not by you.** It follows the profile:
> the `express` developer runs at `medium`, everyone else at `high`. The tester, the
> integration tester, and the reviewer are the adversarial layers, so a session that
> pays for them never makes them think less.

### 6. Process results and close tasks

The workflow returns:

| Field | Shape | Notes |
|---|---|---|
| `session_id` | string | echoed back from `args` |
| `profile` | string | the resolved profile (`express` / `standard` / `full`) |
| `stages_run` | object | `{ implement, test, integrate, review }` — which stages actually ran |
| `integration_expected` | boolean | the wiring expectation this session ran under |
| `passed` | boolean | every stage that ran concluded successfully |
| `rounds` | number | inner test-loop rounds actually run (always 1 for `express`) |
| `review_rework_rounds` | number | rework rounds after the verify stage (0 under `express`) |
| `task_ids` | string[] | the IDs you passed in |
| `dev_reports` | (string \| null)[] | `null` = that developer agent crashed |
| `test_reports` | (object \| null)[] | **structured**: `{ verdict, tasks[], report }`. `null` = crashed. `[]` under `express` |
| `integration_report` | object \| null | **structured**: `{ verdict, findings[], report }`. `null` under `express` or if it crashed |
| `review_report` | object \| null | **structured**: `{ verdict, findings[], report }`. `null` unless `full` ran |
| `review_escalation` | object \| null | present only after max rework rounds; `failed_stages` names which agent objected |
| `session_log` | object[] | per-round trace: one entry per `implement` / `test` / `integrate` / `review` stage, with verdicts and truncated summaries |

> **`passed: true` means less under a shallower profile.** Under `express` it
> means the developer's own gates passed — no independent verification ran, and
> **nothing checked that the code is wired into anything.** Read `stages_run`
> before treating a pass as verified.

> **`passed` is fail-closed across every layer that ran.** The scoped testers, the
> integration tester, and (under `full`) the reviewer must *all* pass. Any one failing — or
> crashing — sends the session to rework. Read `integration_report.verdict` and
> `review_report.verdict` separately; a session can be architecturally sound and still
> unwired.
>
> This includes the **rework rounds**: a rework that breaks a scoped test cannot be rescued
> by a green whole-suite run from the integration tester. A crashed tester never ran its
> mutation checks, and the integration tester is forbidden from re-verifying per-task
> correctness. `review_escalation.failed_stages` names exactly what objected.

> **Verdicts are structured, not scraped.** Testers, the integration tester, and the
> reviewer are all called with a `schema`, so `.verdict` is an enum value — never parse
> prose to decide pass/fail. Read the human-readable markdown from the `.report` field.
>
> **A crashed agent (`null`) is treated as a failure, never as a pass.**
> `parallel()` resolves a thrown thunk to `null`, so fail-closed is the only safe
> reading. A dead integration tester found no wiring defect; that is not the same
> as there being none.

> **Under `standard`, an escalation writes nothing to handoff.** The escalation text is
> written by the *reviewer*, which only `full` runs. When `standard` exhausts its rework
> rounds, `review_escalation.reason` says so — surface it to the user yourself.

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

The verify stage did not pass after `max_review_rounds`. Read
`review_escalation.failed_stages` — it names which agent objected (integration, review, or
both).

Under `full`, the reviewer has already written escalation context to handoff (via
`handoff_save_context` and `handoff_memory_save`). Under `standard` **no reviewer ran, so
nothing was written** — `review_escalation.reason` says so, and surfacing it is on you.

The manager should:

1. Leave tasks in `review` status
2. Record the escalation summary in `notes_append`, including which stage failed
3. Report to the user with the unresolved issues from `review_escalation.final_review`
   and/or `review_escalation.final_integration`
4. Close session (step 7) with `caution` handoff notes referencing the escalation.
   **Under `standard`, write the escalation context yourself** — the next session's step 0
   has nothing to pick up otherwise
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
