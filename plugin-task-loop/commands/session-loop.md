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

### Budget configuration

Per-role turn and tool-call budgets are **advisory** — the workflow injects a `## Budget`
section into each agent's prompt so the agent knows its limits and can report progress
when approaching them. Budgets are NOT enforced at runtime; they guide agent behavior.

| Role                 | Default `max_turns` | Default `max_tool_calls` | Default `soft_wall_time_s` |
| -------------------- | ------------------- | ------------------------ | -------------------------- |
| `developer`          | 80                  | 200                      | 900 (15 min)               |
| `integration-tester` | 60                  | 150                      | 600 (10 min)               |
| `reviewer`           | 40                  | 100                      | 600 (10 min)               |

Pass the `budgets` arg to opt in. Omitting `budgets` entirely produces no budget section
in any prompt (backward compatible). `budgets: {}` opts in with all defaults. Partial
overrides are merged with defaults:

```javascript
budgets: {
  developer: { max_turns: 60 },  // only max_turns overridden; max_tool_calls and soft_wall_time_s keep defaults
}
```

At 90% utilization, agents are instructed to include a progress summary (what's done,
what's remaining, resource breakdown). At 100%, the coordinator receives the agent's
incomplete-work report and decides whether to continue, split the remaining work, or stop.

Budget metadata also appears in `stage_telemetry` entries when configured, enabling
post-session analysis of resource consumption patterns.

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

### 4b. Mark tasks in_progress

Before launching the Workflow, update every task in this session's scope to
`in_progress`:

```
handoff_update_task(task={ id, status: "in_progress" })  // once per task ID in this session
```

Do this here, **outside** the Workflow, not inside it. The workflow's developer
agents run in parallel and are explicitly forbidden from calling
`handoff_update_task` (see each agent's "Handoff context access" section) —
concurrent writes from parallel agents to the same task record is exactly the
failure mode that restriction avoids. The manager is the only writer, and this
is the one point in the flow before the Workflow call where "in progress" is
true and known.

For a bundled task ID (`t1+t2`), update the **underlying** task IDs (`t1`, `t2`)
individually — the same unbundling `session-loop.md` already does for closing
tasks in step 6.

Skipping this step leaves tasks sitting in `todo` for the whole session
duration, which is indistinguishable — to another session, or a human glancing
at the task list — from "nobody has picked this up yet."

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

    // --- Per-role budgets (optional; omit to use no budget section) ---
    // Pass `budgets: {}` to opt in with all defaults, or override per role.
    budgets: {
      developer: { max_turns: 80, max_tool_calls: 200, soft_wall_time_s: 900 },
      'integration-tester': { max_turns: 60, max_tool_calls: 150, soft_wall_time_s: 600 },
      reviewer: { max_turns: 40, max_tool_calls: 100, soft_wall_time_s: 600 },
    },

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
| `passed` | boolean | every stage converged, OR `max_rounds` was reached and unresolved findings were demoted to `pending_followups` instead of failing the session — see below. `false` only when a developer crashed with no report at all |
| `rounds` | number | main-loop rounds actually run (always 1 for `express`) |
| `review_rework_rounds` | number | always 0 (kept for backward compat) |
| `task_ids` | string[] | the IDs you passed in |
| `dev_reports` | (string \| null)[] | `null` = that developer agent crashed |
| `test_reports` | any[] | always `[]` (kept for backward compat; scoped testers removed) |
| `integration_report` | object \| null | **structured**: `{ verdict, findings[], report }`. `null` under `express` or if it crashed |
| `review_report` | object \| null | **structured**: `{ verdict, findings[], report }`. `null` unless `full` ran |
| `pending_followups` | object[] | findings still unresolved when `max_rounds` ran out — one entry per finding: `{ source, task_id, severity, location, problem, crashed }`. `crashed: true` means the agent produced no report at all (BLOCKER, always) — distinct from a normal unresolved finding. Empty when the session converged normally |
| `session_log` | object[] | per-round trace: one entry per `implement` / `test` / `review` stage, with verdicts and truncated summaries |

> **`passed: true` means less under a shallower profile.** Under `express` it
> means the developer's own gates passed — no independent verification ran, and
> **nothing checked that the code is wired into anything.** Read `stages_run`
> before treating a pass as verified.

> **`passed` is fail-closed WITHIN the rework loop, but not across the loop's
> outer edge.** The tester and (under `full`) the reviewer must *both* pass to
> converge; either failing or crashing sends the session to rework. But if
> `max_rounds` runs out with something still unresolved, the workflow does not
> propagate that as `passed: false` — it demotes the remainder to
> `pending_followups` and still reports `passed: true`, so a non-converging
> session completes instead of failing or escalating to a future session.
> Always check `pending_followups` even when `passed` is `true`. Read
> `integration_report.verdict` and `review_report.verdict` to see the actual
> last-round verdicts regardless of what `passed` says.

> **Verdicts are structured, not scraped.** The tester and the reviewer are called
> with a `schema`, so `.verdict` is an enum value — never parse prose to decide
> pass/fail. Read the human-readable markdown from the `.report` field.
>
> **A crashed agent (`null`) is treated as a failure, never as a pass.**

After receiving the Workflow result:

**Check off done_criteria — every round, regardless of pass/fail:**

Each `session-developer` report contains a `### done_criteria progress` section
with one line per criterion, grouped by task: `- <task_id> [index] met: true|false
— <evidence>` (see `agents/session-developer.md`). Read `dev_reports` (every
round the workflow ran, not just the last one) and for every line marked
`met: true`, check it off immediately:

```
handoff_check_criterion(task_id, criterion_index, checked=true)
```

Do this **before** branching on `passed`, and do it even when the session
ultimately fails — a rework round can legitimately satisfy some criteria while
others still need work, and that partial progress should not wait for a
session-wide pass to become visible. The report already names the
**underlying** task_id per line (not the bundled `t1+t2` string), since a
bundled developer's criteria lists are grouped by task, not merged.

**On success (passed: true, pending_followups empty — the loop converged):**

1. (done_criteria already checked off above)
2. Mark tasks as done:
   ```
   handoff_update_task(task={ id, status: "done",
     notes_append: "## session-loop result\n<summary>" })
   ```
3. **Create report tasks for discovered issues** (full procedure in
   `_bug-report-protocol.md`) — before closing:
   - Collect every `### Discovered issues` section from `dev_reports`,
     `integration_report`, and (under `full`) `review_report`.
   - For each item, check `handoff_list_tasks` for a duplicate first.
   - Create a new task via `handoff_update_task` (omit `id`): title prefixed
     `[bug]`/`[improvement]`/`[spec]`, `status: "todo"`, `priority` matching the
     reported severity, `labels: ["found-during-loop", "<type>"]`, and `notes`
     containing the description, `current -> proposed -> benefit`, and the
     originating task/session ID for traceability.
   - Record "Created report task <new_id>" in the session notes (step 7) and
     tell the user "Filed N issues as tXX" in the summary.
4. **Record durable findings as project documents, not just chat history.**
   If the session surfaced a design decision, an architectural discovery, or a
   spec correction that the next session (or a different developer) will need —
   not a one-off implementation detail — persist it with `handoff_doc_save`
   (or `handoff_memory_save` for a short lesson/convention). The manager does
   this on the ordinary success path whenever the session's own reports
   surfaced something worth keeping (this is separate from, and in addition
   to, the follow-up-task doc filed for `pending_followups` below when that
   applies). If nothing rises to that bar, skip it — don't manufacture a doc
   for routine work.
5. Commit:
   ```bash
   # Run the project's quality gates from CLAUDE.md (format, type check, test, lint)
   # Then: git add <changed files> && git commit
   ```
6. Log to session state file

**On failure (passed: false, no developer ever reported):**

This is the one case that still fails the session outright: a developer crashed before any
verification stage could even run (`allDevelopersReported` was false). There is nothing to
converge toward and no findings to file — the implementation itself never landed.

- (done_criteria already checked off above, for whatever passed)
- Leave tasks in `review` status
- Record failure reason and feedback in `notes_append`
- **Still run the Discovered issues step above** — a failed session can still
  surface real out-of-scope bugs the developer noticed along the way.
- Report to user and ask for guidance
- **Still close the session (step 7) regardless**

**No escalation. On `max_rounds` reached without APPROVE/PASS (passed: true,
pending_followups non-empty):**

The workflow does **not** fail the session just because the reviewer or integration tester
was still not satisfied after `max_rounds`. It converts whatever is left into follow-up work
and reports `passed: true` — a session that never fully converges must not become a silent
block on the next session or an infinite rework loop. This is a deliberate policy change from
escalation: no session-reviewer round writes to handoff on your behalf, and none is asked to.
Filing follow-ups from `pending_followups` is entirely the manager's job, done as part of the
ordinary "On success" procedure above, with this addition **before** step 5 (commit):

1. For each entry in `pending_followups` (`{ source, task_id, severity, location, problem,
   crashed }`), check `handoff_list_tasks` for a duplicate first (a finding may repeat
   something the Discovered-issues step already filed).
2. For genuinely new ones, create a follow-up task via `handoff_update_task` (omit `id`):
   - `title`: `[review-followup]` prefix + a concise statement of the problem. If
     `crashed: true`, prefix with `[review-followup][agent-crashed]` instead — this is not a
     reviewed defect, it is an absence of review, and the title must say so.
   - `status: "todo"` (backlog — do not start it in this loop)
   - `priority`: `high` for BLOCKER (this includes every `crashed: true` entry — see
     `extractUnresolvedFindings`), `medium` for MAJOR, `low` for MINOR/NIT
   - `labels: ["found-during-loop", "review-followup"]` (add `"agent-crash"` when
     `crashed: true`)
   - `notes`: the `problem` text, the `location`, which stage found it (`source`), the
     originating task_id if not `"*"`, and this session's ID. For `crashed: true` entries,
     say plainly that the session's actual state is unverified — the crash means no one
     checked it, not that a checker found nothing wrong.
3. Write a short doc via `handoff_doc_save` capturing, for the batch of follow-ups from this
   session: what the unresolved problem(s) actually are, how you'd recommend implementing the
   fix (if you have a view), and any question that needs the user's input before someone can
   pick this up. `problem` is often terse (a truncated report, or a generic crash message) —
   if you do not have enough to propose a fix, say so explicitly in the doc rather than
   inventing a plausible-sounding one; "needs a human to first figure out what broke" is a
   legitimate open question, especially for `crashed: true` entries. Pass `task_ids` with
   every task ID created in step 2 so the doc and the tasks link both ways.
4. Record "Filed N follow-up tasks (tXX, tYY, ...) from unresolved review/test findings,
   see doc <doc_id>" in the session notes (step 7), and tell the user the same in the summary
   — this is not a silent pass. The user should know the session completed with open items.
   If any entry had `crashed: true`, say so explicitly in the summary — the session's actual
   quality is unverified for that stage, which is a stronger caveat than "review requested
   changes."
5. Mark the session's own tasks `done` as usual (this is still the success path) — the
   follow-up tasks are separate, newly-created backlog items, not a reason to leave the
   original tasks in `review`.

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
