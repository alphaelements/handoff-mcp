---
description: Requirements-authoring loop — profile selection, architecture, NFR, parallel scope decomposition, adversarial verification, roadmap and baseline, with a user approval gate between every phase
argument-hint: '<project or feature description> | resume'
---

# Requirement Loop (Requirements Coordinator)

You are the **requirements coordinator**. You do not decompose, check, or review
requirements at scale yourself. Your job is to **run the dialogue phases with the user**
(profile, architecture, NFR), **launch the `requirement-execute` Workflow** for the
parallel authoring and verification phases, **finish the roadmap and baseline yourself**,
and **hand the result to `/session-loop`**. Between every two phases you stop at a
**user approval gate** and run the matching **preflight checklist**.

Same role as the session manager in `session-loop.md`, applied to requirements: Opus,
orchestration only, all state changes through `handoff_*` MCP tools.

## Flow overview

```
Phase 0  Project initialization      (coordinator + user)   -> Gate #0  profile
Phase 1  Architecture design         (coordinator + user)   -> Gate #1  system context + scopes
Phase 2  Shared contracts (NFR)      (coordinator + user)   -> Gate #2  NFR checklist result
Phase 3  Basic requirements          (Workflow: DRAFT+CHECK)-> Gate #3  tree + coverage + lint
Phase 4  Adversarial verification    (Workflow: REVIEW loop)-> Gate #4  final requirements + quality
Phase 5  Roadmap                     (coordinator)          -> Gate #5  tasks + schedule + plan
Phase 6  Baseline and handoff        (coordinator)          -> /session-loop
```

Each gate = **preflight checklist** (`skills/handoff-requirements/references/preflight-checklists.md`)
**+ `AskUserQuestion`** approval. Never enter the next phase on a failed checklist or
without an explicit "approved" answer.

### Which parts run where

| Phase | Executor | Why |
|---|---|---|
| 0-2 | coordinator (you) with the user | needs user decisions; no agent can answer them |
| 3-4 | `requirement-execute` Workflow | per-scope work is independent and parallel; review needs a fresh Opus context |
| 5-6 | coordinator (you) | short, mechanical, and need the user's approval of tasks and schedule |

> **One Workflow run covers Phases 3 and 4.** `requirement-execute` runs
> DRAFT -> CHECK -> REVIEW (-> REWORK, up to `max_rounds`) -> REGISTER (registrar ->
> test-spec-worker -> test-spec check, see "Register stage" below) in a single call and
> returns once. You launch it once (Phase 3), then present Gate #3 from the **first-round**
> worker/checker reports and Gate #4 from the **review** result and final state. See
> "Workflow behaviors you must account for" below for the registrar caveat.

## Configuration parameters

| Parameter | Default | Description |
|---|---|---|
| `coordinator_model` | `opus` | Model for this coordinator |
| `worker_model` | `sonnet` | Set by the `requirement-worker` agent definition |
| `reviewer_model` | `opus` | Set by the workflow (`requirement-reviewer`) |
| `max_rounds` | `2` | Max DRAFT/CHECK/REVIEW gate-loop rounds. Prefer `[trace.requirement_gate]` in config |
| `checker_types` | `['iso29148','nfr','completeness']` | Checks each `requirement-checker` runs |
| `skip_register` | `false` | `true` = skip the registrar stage (register only after the approval gates) |
| `stop_after` | `register` | `elicit` / `check` / `review` / `register`: stop once that stage has completed |
| `response_language` | the user's language | Passed to the workflow so agent reports match the user |

The project may override the gate in `config.toml`:

```toml
[trace.requirement_gate]
fail_on = "error"            # "error" | "warning"
required_checks = []         # subset of iso29148 | nfr | completeness
skip_checks = []
```

Read it with `handoff_get_config` and pass it through to the workflow **verbatim**
(see Phase 3). Never translate or pre-filter it yourself.

## Suspend and resume

The flow spans a long dialog (Phases 0-2), a Workflow run (Phases 3-4) and a direct
close-out (Phases 5-6). The user may stop at **any gate** and pick the flow up in a later
session. All resumable state is one object, `reqflow_state`, stored in the session
`environment` via `handoff_save_context`. It records *where the flow stands*; the scope
content lives in handoff documents (`doc_id`).

### Data model

```json
{
  "reqflow_state": {
    "current_phase": 3,
    "completed_phases": [0, 1, 2],
    "gate_results": { "0": { "passed": true }, "1": { "passed": true } },
    "scopes": [
      { "id": "auth",    "status": "defined",    "doc_id": null },
      { "id": "billing", "status": "registered", "doc_id": "doc-yyy" }
    ],
    "nfr_doc_id": "doc-zzz",
    "parent_task_id": "<task id or null>",
    "workflow_run_id": "<Workflow run id or null>",
    "round": 1
  }
}
```

| Field | Meaning |
|---|---|
| `current_phase` | The phase to **re-enter** on resume (0-6): the phase in progress, or waiting at its gate, when the flow stopped. |
| `completed_phases` | Phases whose gate passed. Never shrinks; a re-entered phase is not listed until its gate passes. |
| `gate_results` | Per-phase gate outcome keyed by the phase number as a string: `{ "passed": true }`, plus `"notes"` for a conditional pass. |
| `scopes[]` | One entry per scope: stable unique `id`, `title`, `doc_id` (the scope's requirement document once one exists, else `null`), `status` (below). |
| `nfr_doc_id` | The NFR document from Phase 2 (`null` before then). |
| `parent_task_id` | The `reqflow` parent task created in Phase 3 (`null` before then). |
| `workflow_run_id` | Run id of the last `requirement-execute` launch. Record it as soon as the launch returns. |
| `round` | Workflow gate-loop round reached in the current Phase 3-4 cycle (`0` before Phase 3). |

**Scope `status` — the one vocabulary** (moves forward only):

| Status | Set when |
|---|---|
| `defined` | Phase 1: scope id/prefix/root fixed |
| `drafted` | Phase 3: worker wrote a usable document (`doc_id` set) |
| `checked` | Phase 3: checker verdict `PASS` (or the user accepted the findings at Gate #3) |
| `approved` | Phase 4: reviewer passed this scope / the user accepted it at Gate #4 |
| `registered` | Phase 4: the run returned `registered: true` and the registrar covered this scope |

A scope is **complete** only when its status is `registered`. **`pending_scopes`** is not
stored; it is derived on resume as every scope whose status is not `registered`. If the
user rejects Gate #4 or #5 and the registrar's tasks are cancelled through the parent task,
demote the affected scopes to `checked`.

### Saving state

`handoff_save_context` **replaces** `environment` wholesale. Never save only
`{ reqflow_state }`: take the current `environment` from `handoff_load_context`, set/replace
its `reqflow_state` key, and save the merged object, or whatever else the environment carried
is erased.

Save with `session_status="active"` at every point where the flow can be interrupted:

1. Before presenting any gate and waiting for the reply (the state shows that gate as pending).
2. After each gate resolves (update `completed_phases`, `gate_results`, advance `current_phase`).
3. After each scope changes status.
4. Right after the Workflow launch returns (`workflow_run_id`), and after each Workflow
   result is processed (`round`, scope statuses).

### Suspending

Every gate's `AskUserQuestion` offers **Suspend** next to its other options; also treat a
free-text "pause", "stop for now" or "continue later" as Suspend. (**Stop** means abandon
the flow, see below.) On Suspend:

1. Start no further agent or Workflow. If a Workflow is running, let it return, or note its
   `workflow_run_id`; do not discard partial output.
2. Set `current_phase` to the phase you were in (the gate you were waiting on, not the next
   one); scope statuses reflect only work that finished.
3. Save with the merged environment and **close** the session:

   ```
   handoff_save_context(
     session_status="closed",
     summary="Requirements flow suspended at Phase <n> (<k>/<m> scopes registered)",
     handoff_notes=[{ category: "suggestion",
                      note: "Run /requirement-loop to resume from Phase <n>; pending scopes: <ids>" }],
     environment=<existing environment + reqflow_state>)
   ```

4. Tell the user which phase and scopes are pending and that `/requirement-loop` resumes the
   flow. Stop; do not continue the loop.

**Stop** (abandon): confirm once, then save `session_status="closed"` with the merged
environment **without** `reqflow_state`, and a `caution` note naming the documents and the
`reqflow` parent task left behind (cancel the parent task if the user asks). A crash or
context loss is handled like Suspend: the last save is the resume point; work after it is
redone, never skipped.

### Resuming

Done in Step 0, after `handoff_load_context`:

1. Look for `reqflow_state` in `environment` (active session) and, if absent, in
   `previous_session.environment` (closed session). If neither has it, this is a fresh run:
   start at Phase 0. If the user passed a new topic while a state exists, ask whether to resume
   or start over; never silently discard it.
2. **A finished flow is not resumed.** If `completed_phases` contains `6`, or every scope is
   `registered` and `current_phase` is 6, the baseline already exists: tell the user, and do
   not re-enter Phase 6 (that would create a second baseline). Offer a fresh run only if
   the user asks.
3. **Validate before trusting it.** `current_phase` is an integer 0-6; every `scopes[].status`
   is one of the five values; `completed_phases` does not contain `current_phase`. If anything
   is malformed, show the problem and ask the user; do not guess a phase.
4. Check that each scope `doc_id` still resolves (`handoff_doc_get`). A missing document demotes
   the scope to `defined` (no usable document remains); report the demotion.
5. Show a one-screen summary (current phase, completed phases, scope table id / status, `round`)
   and ask for confirmation, with "start over" as the alternative.
6. Re-enter `current_phase`, re-running its preflight checklist and re-asking its gate. Skip every
   phase in `completed_phases` unless the user explicitly asks for it:
   - **Phases 0-2** (dialog): keep approved decisions; do not re-ask what `gate_results` records
     as passed.
   - **Phases 3-4**: launch `requirement-execute` with **only the pending scopes** (`scopes[]`
     filtered to status != `registered`) and `context.nfr_doc_id` from the state. Registered
     scopes are never re-drafted, re-checked or re-registered. To reuse the cached `agent()`
     results of a stopped run, pass `resumeFromRunId = workflow_run_id` **and the same `args`**
     as the original launch (args are not inherited and are part of the cache key). If the
     original args cannot be reproduced (the pending scope list changed), launch a fresh run.
   - **Phases 5-6**: run directly; they are idempotent (Phase 5 previews before applying; Phase 6
     checks the baseline list before creating one).

## Detailed procedure

### Step 0. Establish session (MUST run at the start of every session)

```
handoff_load_context
-> Review previous decisions / next_actions / handoff_notes
-> Look for reqflow_state in environment, then previous_session.environment
   (see "Suspend and resume" -> Resuming). If found and not finished, resume from
   current_phase instead of starting Phase 0.
-> If no active session:
  handoff_save_context(session_status="active",
    summary="Requirement loop: <project>", label="Requirements: <brief description>")
```

Then read `skills/handoff-trace/SKILL.md` (trace tools, profiles) and
`skills/handoff-requirements/references/requirement-tree.md` (tree structure, INVEST) before
Phase 1.

---

### Phase 0: Project initialization

**Executor**: you, with the user. No agent.

1. `handoff_get_config` — read `[trace] profile`, `layers`, `id_prefixes`, `done_guard`,
   `requirement_gate`.
2. `handoff_trace_report` — if it returns `DIAG-T001` (no profile/layers configured),
   the project is not initialized for the V-model yet; Phase 0 must fix it.
3. Ask the user for the project in **one sentence** (purpose and scope) and the main actors.

   **Requirements specification from `/spec-loop` (automatic reference, optional).** Before
   asking, and before the brainstorm report below, look for the specification that
   `/spec-loop` Phase 4 saves (`doc_type: spec`, tags `spec-loop` and `requirements-spec`, plus
   `approved` once the user approved it at its final gate):
   - If the user or the invocation passed a `spec_doc_id`, use it. Otherwise search with
     `handoff_doc_list(tags=["spec-loop", "requirements-spec"])` and `handoff_doc_get` each hit;
     keep only those that also carry the `approved` tag (a draft, suspended or stopped
     specification is not input). More than one approved: ask the user which one applies.
   - If one exists, read it and **do not re-ask what it already answers**. Import:
     - **architecture**: the system-context / component diagram (Mermaid) and its description.
       Phase 1 starts from it: reuse it for the system-context diagram and the `SYS-*` split
       instead of interviewing from scratch, and ask the user only to confirm or correct it.
     - **screens**: the screen list, mockup references and the screen-transition diagram. This
       makes the project a GUI project: the screens feed "GUI project flow" under Phase 3 (the
       mockup paths are the `mock:` values; never invent one the specification lacks). A
       specification that records "Phase 2 skipped: no GUI" means no GUI input.
     - **use cases**: the use cases (actor, preconditions, main / alternative / exception
       scenarios). They are the Phase 1 "main use cases" input and the `UC-*` references in the
       scopes' `instructions`; do not rewrite them.
     - **scope, constraints and open questions**: purpose, actors, in/out of scope with their
       `BS-*` references, constraints and the open questions (these go to the Phase 1 interview).
   - Show the user a short summary of what was imported and ask them (`AskUserQuestion`) to
     confirm or correct it; the confirmed purpose and actors answer this step. The specification
     is read-only input; do not edit it.
   - The specification keeps the `BS-*` references of the brainstorm report it was built from. If
     those items exist as layer items (see the brainstorm paragraph below), `refines:` links still
     point to them.
   - No approved specification exists (none found, or only drafts): continue with the brainstorm
     report below, or with manual input. `/spec-loop` is optional; its absence is never an error.

   **Brainstorm report (automatic reference).** Before asking, look for a brainstorm report
   produced by `brainstorm_finalize` (a document tagged `brainstorm-report`; once the project
   declares the `brainstorm` layer, finalize also files it under `layer: brainstorm`):
   - If the user or the invocation passed a `report_doc_id`, use it. Otherwise search with
     `handoff_doc_list(query="brainstorm")` and `handoff_doc_get` each hit; keep those tagged
     `brainstorm-report` or whose `layer` is `brainstorm`. More than one: ask the user which
     one applies.
   - If one exists, read it and **do not re-ask what it already answers**: take the purpose,
     scope (section 5 In-scope / Out-of-scope), target users and actors from it and only ask
     the user to confirm or correct them. Use it as input to this phase: its priorities and
     constraints justify the profile choice (step 4), its "要件定義への申し送り" section seeds
     Phase 1 (architecture) and the NFR candidates, and its open questions go to the Phase 1
     interview.
   - Make the brainstorm items reachable: the `brainstorm` layer is not in any built-in profile.
     If `handoff_get_config` shows no `trace.layer` entry with `id = "brainstorm"`, declare it
     (`level = 0`, `verification = false`, `id_prefixes = ["BS"]`; see
     `skills/handoff-trace/references/config.md` §4.2) and, in step 5, set `trace.profile` to a
     custom profile that extends the chosen built-in one and adds `"brainstorm"` to its
     `layers` (e.g. `trace.profiles.standard-bs` = `{"extends": "standard", "layers":
     ["brainstorm", "requirement", "basic_spec", "acceptance", "system_test"]}`), all through
     `handoff_update_config`. `trace.layer` is replaced as a whole, so send the full list.
     If the report has no layer yet (it was finalized before the layer was declared:
     `handoff_doc_get` shows `layer` empty, and `brainstorm_finalize` warned about it), set it
     now with `handoff_doc_save(doc_id=<report_doc_id>, layer="brainstorm")`; that turns its
     `BS-*` headings into items.
   - From Phase 1 on, every requirement derived from the report links back to it with
     `- refines: BS-SCOPE-NNN` (the `BS-*` item ids in the report: `BS-SCOPE-NNN` for In-scope
     entries, `BS-PRIORITY-NNN` for priorities, `BS-HINT-NNN` for hand-off hints, as listed in
     the `item_ids` of the `brainstorm_finalize` result), so `handoff_trace_report`
     shows brainstorm -> requirement traceability. Brainstorm items have no verification
     pair: they report horizontal coverage `na` and never need an acceptance item.
   - The report has a `## 8. GUI 設計概要` section (`screens`, `screen_flow`, `ux_policy`; the
     brainstorm ran with `project_type: gui-*`): this is a GUI project, follow "GUI project flow"
     under Phase 3.
   - No brainstorm report exists: continue as usual.
   - When an approved specification was imported above, it already carries the report's content
     (scope, priorities, hints, screens). Still use the report for the layer, `refines:` and
     profile steps if it exists, but do not ask the user to re-confirm what the specification
     settled.
4. **Propose a profile** using this table, state the reason, and let the user choose:

   | Project | Profile | Layers you will maintain |
   |---|---|---|
   | small tool, AI-driven, lightest V | `minimal` | requirement (verification items generated) |
   | product with specs and system tests | `standard` | requirement + basic spec + acceptance + system test |
   | safety / regulated, full decomposition | `full` | all six layers |
   | bug-fix stream | `bugfix` | reproduction (requirement) + regression test (acceptance) |

   Default recommendation: `standard`. Use `full` only when the user names a regulated or
   safety domain. A project that already has a profile keeps it unless the user changes it.
5. Apply the choice:

   ```
   handoff_update_config(updates: {
     "trace.profile": "standard",
     "trace.done_guard": "warn"        // "block" only if the user asks to enforce
   })
   ```

6. Re-run `handoff_trace_report` and confirm `DIAG-T001` is gone.

**Gate #0 — preflight "Gate 0 to 1"** (initialization complete): profile configured,
purpose stated in one sentence, actors listed. (The requirement-layer document itself is
created in Phase 1.)

`AskUserQuestion`: show profile, layers, done_guard and the one-sentence purpose; options
"Approve / Change profile / Suspend / Stop".

Save `reqflow_state` (`completed_phases: [0]`, `current_phase: 1`).

---

### Phase 1: Architecture design (SYS requirements + scope split)

**Executor**: you, with the user. No agent.

1. **Interview** the user about the system: its overview, boundary (what is inside / outside),
   external systems, actors, and the main capabilities. Batch the questions; do not drip-feed.
2. **Main use cases**: write each primary use case with the **use-case template** in
   `skills/handoff-requirements/references/templates.md` ("Use-Case Summary"). Keep them in a
   plain document (`doc_type: "spec"` is fine), not a layer document.
3. **Create the requirement-layer document** (one root per major capability):

   ```
   handoff_doc_save(slug="<project>-architecture", title="<Project> Architecture Requirements",
                    layer="requirement", body="...")
   ```

   - Top-level items use the `SYS-*` prefix; one `SYS-nnn` per major capability.
   - Embed the **system-context diagram** (Mermaid in a fenced block) showing actors and
     external systems.
   - Pass `layer` explicitly on every `handoff_doc_save`.
4. **Split into scopes**: each scope owns one ID prefix and one root requirement, and becomes
   the unit of parallel work in Phase 3. Register new prefixes before use:

   ```
   handoff_update_config(updates: { "trace.id_prefixes.requirement": ["SYS", "<SCOPE1>", ...] })
   ```

   Define a `SCOPE-*` entry per scope (id, title, prefix, root `SYS-nnn`) in the document.
   Scopes must not overlap; if two scopes would edit the same items, merge or re-cut them.
5. `handoff_trace_tree(layer="requirement", depth=1)` to confirm the roots, and
   `handoff_trace_lint` to confirm `counts.error = 0`.

**Gate #1 — preflight "Gate 1 to 2"** (architecture complete), plus: at least one scope defined.

`AskUserQuestion`: show the system-context diagram, the SYS list and the scope table
(id / title / prefix / root); options "Approve / Revise scopes / Suspend / Stop".

Save `reqflow_state` (`scopes` with `status: "defined"`, `completed_phases: [0,1]`, `current_phase: 2`). Suspend instead keeps `current_phase: 1`.

---

### Phase 2: Shared contracts (NFR)

**Executor**: you, with the user. No agent.

1. Present the **NFR checklist** (`skills/handoff-requirements/references/nfr-checklist.md`),
   the eight ISO/IEC 25010 characteristics: performance efficiency, compatibility, usability,
   reliability, security, maintainability, portability, functional suitability.
2. For **each** characteristic, ask the user whether it applies. Record the decision:
   - applicable -> write NFR items (below);
   - not applicable -> record the reason:
     `handoff_trace_lint(action="nfr_check", review={"portability": "not_applicable"})`.
3. Write the `NFR-*` items into one requirement-layer document
   (`handoff_doc_save(slug="<project>-nfr", layer="requirement", ...)`). Each NFR must have:
   - a **numeric criterion** with unit and condition (no "fast", "secure", "easy");
   - `- method: manual` or `- method: auto`;
   - an acceptance criterion.
   Remember its id as `nfr_doc_id`; Phase 3 passes it to the workers and checkers.
4. `handoff_trace_lint(action="nfr_check")` — every one of the eight categories must be
   `applicable` (has items) or reviewed `not_applicable`; list any `unclassified` item and fix
   its id. Then `handoff_trace_lint(rules=["missing_acceptance","ambiguous_word"])` must be quiet
   for the NFR items.

**Gate #2 — preflight "Gate 2 to 3"** (shared contracts complete).

`AskUserQuestion`: show the eight-category table (status, item ids, reasons for N/A) and
the numeric targets; options "Approve / Add or change NFRs / Suspend / Stop".

Save `reqflow_state` (`nfr_doc_id`, `completed_phases: [0,1,2]`, `current_phase: 3`).

---

### Phase 3: Basic requirements (parallel DRAFT + CHECK)

**Executor**: `requirement-execute` Workflow.

1. **Create the parent task** that the registrar will attach everything to, outside the
   workflow, so the whole batch can be reviewed or cancelled as a unit:

   ```
   handoff_update_task(task={ title: "Requirements: <project>", status: "in_progress",
                              labels: ["reqflow"] })     // -> parent_task_id
   ```

2. **Assign workers**: one `scopes[]` entry per Phase 1 scope — never two workers on the same
   branch/document (see "One branch, one document" in `requirement-tree.md`).
3. **Launch the Workflow.**

   **Always use `name: "handoff-task-loop:requirement-execute"`. Never write an inline script.**
   Inline scripts bypass the agent definitions (`requirement-worker` = Sonnet,
   `requirement-reviewer` = Opus). All customization goes through `args`.

   > **Resuming a run**: `resumeFromRunId` does NOT auto-inherit `args`. Pass the same `args` again.

   ```javascript
   Workflow({
     name: 'handoff-task-loop:requirement-execute',
     args: {
       parent_task_id: '<parent task id>',
       scopes: [
         {
           id: 'auth',                       // unique scope id
           title: 'Authentication',
           parent_doc: '<project>-architecture',   // document holding the parent SYS item(s); worker reads only
           depth_target: 'leaf',
           layer: 'requirement',
           instructions: 'Prefix AUTH-. Root SYS-001. Decompose Feature -> Condition -> Leaf; every leaf INVEST-checked with GWT/EARS acceptance criteria. Use UC-001..UC-003 as input.',
         },
         // ...one per scope
       ],
       max_rounds: 2,                        // from [trace.requirement_gate]/user; default 2
       response_language: '<user language>',
       context: {
         branch: '<current branch>',
         nfr_doc_id: '<Phase 2 NFR doc id>',
         requirement_gate: { /* the [trace.requirement_gate] table from handoff_get_config, verbatim; null/omit = defaults */ },
       },
     },
   });
   ```

   Record `workflow_run_id` in `reqflow_state`.

   #### GUI project flow (`project_type = gui-app`, or any `gui-*`)

   Applies when the brainstorm report carries GUI topics (`screens`, `screen_flow`, `ux_policy`,
   section `## 8. GUI 設計概要`). The workflow itself is unchanged; the GUI-specific input travels
   in each scope's `instructions` string, because that is the only channel from you to the
   `requirement-worker`. The attribute syntax lives in `skills/handoff-requirements/SKILL.md` §4
   ("GUI requirement attributes (VRT)"); do not restate it, point to it.

   1. **Hand the GUI topics to the worker.** For each scope that owns screens, append to
      `scopes[].instructions`: the screens of that scope from the report (name, function,
      priority, `wireframe_ref`), the relevant `screen_flow` lines and the `ux_policy` lines, plus
      this order: "Each screen requirement must carry GUI attributes per handoff-requirements §4:
      `mock:` (the wireframe/mock path from the report), and `golden:`, `viewport: <W>x<H>`,
      `capture_method:` where the report or `config.toml [gui]` settles them. Never invent a
      `mock` path; if the report has no mock for a screen, leave the line out and say so in the
      worker report." Scopes without screens get no GUI instructions.
   2. **Worker output.** The `requirement-worker` writes `- mock:` / `- golden:` / `- viewport:`
      / `- capture_method:` lines directly under the screen item's heading (strict attribute
      position). At Gate #3, check each screen item with `handoff_doc_req_list` (the fields are
      returned as `mock` / `golden` / `viewport` / `capture_method`): a screen without `mock`
      gets no VRT task, so send the scope back or record the gap with the user's agreement.
   3. **VRT tasks are generated, not hand-written.** Once the requirements exist,
      `handoff_trace_tasks` creates an extra `[VRT] <id> <title>` verification task next to the
      normal task of every item that has a `mock` line. In Phase 5 these come out of the same
      preview/apply call as the other tasks (the registrar's stage also creates them); do not
      create VRT tasks manually and do not apply a second time for scopes already registered
      (see Phase 5, step 2).
   4. **Tie the generated tasks to the release scope.** Pass the scope's milestone as
      `default_milestone` on every `handoff_trace_tasks(mode="apply")` call so the normal and VRT
      tasks both get `schedule.milestone` (an item's own `- milestone:` line, else its document's
      milestone, still wins over the default); Phase 5 step 1 (milestone integration) decides the
      name, and the "Assign milestones" bullet backfills registrar-created tasks.

4. **Read the result** (shape below). Phase 3 results come from round 1 of
   `worker_reports` / `checker_reports` (`round: 1`) and the documents they wrote.

   | Field | Notes |
   |---|---|
   | `passed` | `true` = the reviewer passed within `max_rounds`. `false` = did not converge (not a crash) |
   | `registered` | `true` = the registrar ran and returned a usable result |
   | `rounds` | gate-loop rounds used |
   | `scopes_processed[{scope_id, doc_id}]` | `doc_id: null` = the worker produced no usable document |
   | `worker_reports[{round, scope_id, report}]` | `report: null` = the worker crashed |
   | `checker_reports[{round, scope_id, doc_id, report}]` | `{findings[], verdict: PASS|REDO}` |
   | `review_report` | `{verdict, findings[], rework_notes}` or `null` |
   | `registrar_report` | `{tasks_created, links_created, lint_result, scaffold_docs[]}` or `null` |
   | `test_spec_report` | `{items_completed, items_flagged, remaining_tbd, warnings[]}` from the test-spec-worker, or `null` (not run / crashed) |
   | `test_spec_check_reports[{round, doc_id, report}]` | ISO 29119 check of each scaffolded test-specification document: `report` is `{findings[], verdict}`, `null` = the checker crashed; `[]` when nothing was scaffolded |
   | `pending_followups[]` | unresolved BLOCKER/MAJOR findings, including `source: 'workflow'` crashes |
   | `requirement_log[]` | per-round trace |

   > **A `null` report is a failure, never a pass.** A missing worker/checker/reviewer result
   > is a crashed agent, not an absence of defects.

5. **Independently verify** (do not trust agent reports alone):
   - `handoff_trace_tree(scope="<PREFIX>-")` per scope; `handoff_trace_tree(filter={gaps_only:true})`
     returns no tree-structure finding.
   - `handoff_trace_lint` has `counts.error = 0`.
   - `handoff_trace_lint(rules=["missing_acceptance","leaf_without_acceptance"])` is quiet.
   - The per-item quality aspects (`handoff_trace_lint(action="quality_prompt")`) are covered by
     the checkers' `iso29148` results; for a scope whose checker reported `PASS` with MINOR
     findings, spot-check those items before presenting the gate.

**Gate #3 — preflight "Gate 3 to 4"** (basic requirements complete).

`AskUserQuestion`: show per scope the requirement tree (`trace_tree`), item/leaf counts,
checker verdict and findings, and lint counts; options "Approve / Send a scope back / Suspend / Stop".
"Send a scope back" means re-running the workflow for that scope only (a new run with just that
`scopes[]` entry and the user's extra instructions), then re-presenting the gate.

> **Registration must follow approval.** `requirement-execute` accepts `skip_register: true`
> (the gate loop runs but the registrar stage does not) and `stop_after`
> (`elicit` | `check` | `review` | `register`; default `register`). Launch with
> `skip_register: true` (or `stop_after: "review"`) so no tasks or verification items exist before
> Gate #3/#4 is approved, then register after approval. A targeted re-run ("Send a scope back" /
> "Run another targeted round") still gives the reviewer only the named scope's documents, so run a
> full-scope pass afterwards for cross-scope review; and if an earlier run already registered,
> list the existing tasks under `parent_task_id` and cancel or reuse them first, because a new
> registrar run can create duplicates.

Save `reqflow_state` (`scopes[].status` set to `drafted` / `checked` per the status table, `completed_phases: [0..3]`, `current_phase: 4`).

---

### Phase 4: Adversarial verification (CHECK + REVIEW gate loop)

**Executor**: the same Workflow run (its REVIEW stage and rework rounds).

Within the run, the `requirement-reviewer` (Opus) judges **all** documents together for
duplicates, contradictions, omissions and granularity, and the checkers add the set-level
`completeness_set` check. `REDO` re-drafts and re-checks **only the flagged scopes**, then
reviews again, up to `max_rounds` (default 2). The gate cannot be bypassed: a reviewer
`PASS` is overridden to `REDO` while any checker finding is at or above `fail_on`.

After the run returns, read `review_report`, `pending_followups`, `requirement_log`:

- **`passed: true`** — converged. Present the final state at Gate #4.
- **`passed: false`** — did not converge in `max_rounds`. **Do not re-run silently.**
  Present the unresolved findings at Gate #4 and let the user pick: accept and file the
  findings as follow-up tasks, run one more targeted round for the named scopes, or stop.
  A `crashed`/`source: 'workflow'` entry means an agent produced nothing — call it out explicitly.
- **Always** check `pending_followups`, even when `passed: true`: unresolved MAJOR findings are
  demoted to follow-ups, not failures.

File each follow-up (duplicate-check against `handoff_list_tasks` first) with
`handoff_update_task` (omit `id`): title `[req-followup] <message>`, `labels: ["found-during-loop", "req-followup"]`,
priority `high` for BLOCKER, `medium` for MAJOR, notes with scope, item, source stage and
the run id. Follow `agents/_bug-report-protocol.md` for any other issue you discover.

**Gate #4 — preflight "Gate 4 to 5"** (detailed requirements and verification design).
Apply it according to the project profile: for `minimal`/`bugfix`, acceptance criteria already
yield verification items, so the basic-spec/system-test rows are N/A — say so explicitly.
Under `standard`/`full`, `handoff_trace_report` must show horizontal coverage and the reviewer
(this run's `review_report`) must have passed.

`AskUserQuestion`: show the final requirement list per scope, the quality report
(checker verdicts, review findings, follow-ups filed), and `rounds` used; options
"Approve / Run another targeted round / Suspend / Stop". Read the "Registration must follow approval" note under Gate #3
before offering the targeted round (duplicate registration, no cross-scope review).

Save `reqflow_state` (`round`, `scopes[].status` set to `approved`, or `registered` for scopes the run
registered with `registered: true`, `completed_phases: [0..4]`, `current_phase: 5`).

#### Workflow behaviors you must account for

- **The registrar runs inside the workflow, before Gate #4.** On `passed: true` the run ends with
  `requirement-registrar` creating verification items (`trace_scaffold`) and tasks
  (`trace_tasks`) under `parent_task_id`. Treat those tasks as **proposed until Gate #5**:
  Phase 5 verifies them rather than creating them again, and if the user rejects at Gate #4 or
  #5, cancel them as a unit through the `reqflow` parent task. This is why Phase 3 creates a
  dedicated parent task.
- **`registered: false` with `passed: true`** means the documents were approved but nothing was
  registered. Phase 5 performs the registration itself.
- **`passed: false` registers nothing.** Do not enter Phase 5 until the user resolves Gate #4.

#### Register stage

The Register stage is **one stage of the same workflow run** (phase numbering is unchanged:
Phase 5 Roadmap and Phase 6 Baseline stay exactly as they are). It runs four steps in order,
only when the gate loop converged (`passed: true`) and registration is not skipped
(`skip_register` / `stop_after`):

1. **`requirement-registrar`** scaffolds verification items into the `acceptance` layer and,
   under `standard` / `full`, the `system_test` layer (`trace_scaffold`), then generates the
   `implements` tasks (requirement layer) and the `executes` tasks (only the scaffolded items,
   not project-wide) with `trace_tasks`. It returns `scaffold_docs`, the verification-layer
   documents that received items.
2. **`test-spec-worker`** completes the `(TBD)` preconditions / steps / environment of every
   scaffolded item, marking guesses `(要確認)`. It is skipped when `scaffold_docs` is empty
   (`minimal` / `bugfix` profiles).
3. **`requirement-checker`** (one per scaffolded document, in parallel, check type `test_spec`)
   validates the test specifications against ISO/IEC/IEEE 29119: all seven attributes present
   (objective, preconditions, steps, expected_result, environment, method, priority), no `(TBD)`
   or `(要確認)` left, `verifies` / `from` links resolve, `test_refs` present for `method: auto`.
   Results are returned as `test_spec_check_reports`. It runs even when the test-spec-worker
   crashed, so leftover placeholders are detected.
4. **`trace_lint`** is the registrar's closing check (`task_unlinked = 0`), reported in
   `registrar_report.lint_result`.

A failure in steps 2-3 never un-registers anything: it leaves `registered: true` and surfaces in
`pending_followups` (`source: 'workflow'` for a crashed agent or leftover `(TBD)` placeholders,
`source: 'test_spec_checker'` for checker BLOCKER/MAJOR findings, each carrying its `doc_id`).
Present them at Gate #4 with the other follow-ups and file them as `[req-followup]` tasks.

---

### Phase 5: Roadmap

**Executor**: you. Registration was proposed by the workflow; you verify, schedule and document it.

1. **Milestone integration** (REQ-MS-PIPE-002) - do this **before** any task is applied, so the
   milestone names that tasks inherit are registered and the requirement items carry them:
   1. **Collect** the milestone names the scopes use: the roadmap/scope documents from Phase 3 and
      every requirement item's `- milestone: <name>` attribute line
      (`handoff_doc_req_list(category=<prefix>, milestone_filter=<name>)` answers "which items belong
      to milestone X"; items without one inherit the document-level `milestone`).
   2. **Detect undefined ones**: compare against `handoff_list_milestones()` (config.toml
      `[milestones.*]`). Any name used by a scope but absent from config.toml is undefined.
   3. **Propose creation behind a user gate**: `AskUserQuestion` listing each undefined name with a
      suggested `date`/`order`/`depends_on` ("Create all / Edit / Skip"). On approval call
      `handoff_add_milestone(name="<name>", order=<n>, depends_on=[...])` per name. Never create a
      milestone without the user's answer; if the user skips, the scope's items keep the unregistered
      name and `doc_save` will return a warning - record it as a Phase 4 follow-up.
   4. **Stamp the requirement items**: every requirement SubItem must resolve to a milestone. Put
      `- milestone: <name>` on the item's attribute line in the document body, or - when a whole
      scope document ships in one milestone - set it once with
      `handoff_doc_save(doc_id=<scope doc id>, milestone="<name>")` (an item's own line wins over the
      document value; pass `milestone=""` to clear). Re-run
      `handoff_doc_req_list(category=<prefix>, milestone_filter=<name>)` per scope to confirm the count
      matches; an item with no effective milestone is a gap - ask the user rather than guessing.
   5. **Remember the default**: choose the milestone that most of a scope's tasks belong to as that
      scope's `default_milestone`; step 2 passes it to `handoff_trace_tasks` when it applies tasks.
2. **Tasks**:
   - `handoff_trace_tasks(select={layers:["requirement"]}, parent_id=<parent_task_id>, mode="preview")`.
   - **If the run returned `registered: true` for a scope, call `trace_tasks` for that scope in
     `mode="preview"` only — never `mode="apply"`.** The registrar already created its tasks;
     applying again is the duplicate-task bug (318 duplicates in the pochi-deck dogfooding).
     `trace_tasks` also skips an item that already has a task by requirement link or by a
     `"{stable_id} ..."` title (`skipped[].reason`: `existing_link` | `title_match`), but do not
     rely on that guard — skip the apply call.
   - Expected after a registered run: `planned` is empty (existing tasks are in `skipped`).
     Anything in `planned` is a requirement still missing a task — show it to the user and,
     on approval, `mode="apply"` **for those specific items only** (`items=[...]`). If
     `registered` was `false`, this preview is the whole task list: review it with the user,
     then `mode="apply"`. Every apply call passes the scope's milestone:
     `handoff_trace_tasks(items=[...], parent_id=<parent_task_id>, mode="apply", default_milestone="<name>")`,
     so each created task gets `schedule.milestone` automatically. Tasks the registrar already created
     inside the workflow have no milestone yet - the "Assign milestones" bullet below backfills them.
   - Link tasks to requirements: `trace_tasks` already writes the requirement link; additionally
     set `requirement_ids` with `handoff_update_task(task={id, requirement_ids:[...]})` for any task
     the registrar missed (`handoff_trace_next` lists `create_task`/`link` gaps).
   - **Estimate hours per scope** (mandatory; a uniform `0.5h` for every task is forbidden). Set
     `schedule.estimate_hours` from each task's requirement complexity (number of leaf requirements it covers):
     - leaf task (1-2 leaf requirements): 0.5-1h
     - medium task (3-5 leaf requirements): 1-2h
     - large task (6+ leaf requirements): 2-4h

     Count a task's leaf requirements with `handoff_doc_req_list(task_id=<task id>)` (it lists the
     requirement SubItems linked to that task; count the leaves, i.e. items that are not `kind: group`).
     Apply all of them in one call (note: the array is `updates` and the per-item key is `task_id`):
     `handoff_bulk_update_tasks(updates=[{task_id, schedule:{estimate_hours:<h>}}, ...])`.
   - **Assign milestones**: read which milestone (the MVP-0 / MVP-1 / v1 ... defined in Phase 3) each
     requirement belongs to - its effective `milestone` from `handoff_doc_req_list` (stamped in step 1),
     falling back to the roadmap/scope documents - then set it on the matching task in bulk (this
     backfills registrar-created tasks and overrides the `default_milestone` for items that differ):
     `handoff_bulk_update_tasks(updates=[{task_id, schedule:{milestone:"<milestone>"}}, ...])`. Estimates and
     milestones may be merged into one `schedule` object per task. A task with no milestone is a gap -
     ask the user rather than leaving it blank.
   - **Derive cross-scope dependencies automatically** before asking the user:
     1. For each scope's requirement items, read their `refines` links with `handoff_trace_slice`.
     2. If a requirement of scope A refines a requirement of scope B, then scope A's tasks depend on
        scope B's tasks (they get B's task ids in `dependencies`).
     3. Set the derived dependencies **per task** with `handoff_update_task(task={id, dependencies:[...]})`
        (it rejects cycles). **Do not use `handoff_bulk_update_tasks` for dependencies** - it has no
        `dependencies` field and silently drops it while reporting success. After setting them, verify
        with `handoff_get_task` (or `handoff_list_tasks`) that `dependencies` is non-empty on the tasks you changed.
     4. Present the derived list to the user for confirmation (accept / remove / add manual ones).
   - Then ask the user for priorities and any remaining dependencies; apply them with
     `handoff_update_task` (dependencies always via `handoff_update_task`; priorities may use
     `handoff_bulk_update_tasks(updates=[{task_id, priority}, ...])`).
3. **Schedule**:
   - `handoff_auto_schedule(dry_run=true)`, show the proposed dates, get approval, then
     `handoff_auto_schedule(dry_run=false)` — **after the last task change**.
   **Note**: `auto_schedule` uses raw `estimate_hours` directly. The `settings.ai_estimate_multiplier`
   is applied only in `handoff_get_metrics` / `handoff_get_capacity` reports, never during scheduling.
4. **Roadmap document**:
   ```
   handoff_doc_save(doc_type="plan", slug="<project>-roadmap", title="<Project> Roadmap", body="...")
   ```
   (`layer` is inferred from `doc_type` when `[trace] auto_layer` / `plan_default_layer` is set;
   otherwise pass the layer the project's profile uses for plans, or leave it out.)
   **Never hard-code the task count.** Fetch it with `handoff_list_tasks(parent_id=<parent_task_id>)` and
   write it into the plan document as `Auto-computed: N tasks total (X todo, Y done)`. A hand-typed
   number drifts from the real task list and causes inconsistencies.

   Contents: milestones, task ordering rationale, a Mermaid `gantt` of the scheduled tasks, the
   dependency list, and the open follow-ups from Phase 4. Add a SubItem per task and link it
   (task order/priority is propagated from the plan's SubItem order by `auto_schedule`).

**Gate #5 — preflight "Gate 5"** except the baseline row (taken in Phase 6): plan document
exists, every requirement that needs implementation has a task, schedule computed after the
last task change.

`AskUserQuestion`: show the task list with priorities, the Gantt chart and the plan document id;
options "Approve and baseline / Adjust roadmap / Suspend / Stop". Record the approval in the plan document
or the session decisions.

Save `reqflow_state` (`completed_phases: [0..5]`, `current_phase: 6`).

---

### Phase 6: Baseline and handoff to session-loop

**Executor**: you. **No further user gate** — Gate #5 already approved the plan.

1. `handoff_trace_report` — confirm final coverage and that no layer regressed.
2. `handoff_trace_baseline(action="list")` — if a baseline tagged `reqflow-<project>-v1` already
   exists (a resumed Phase 6), reuse it; otherwise
   `handoff_trace_baseline(action="create", tag="reqflow-<project>-v1")` and list again to confirm.
3. Mark the `reqflow` parent task `done` (its children stay `todo` for `/session-loop`), with
   `notes_append` listing the baseline id and plan doc id.
4. Close the session so `/session-loop` can pick up cleanly:

   ```
   handoff_save_context(
     session_status="closed",
     summary="Requirements phase complete: <N> requirements, <M> tasks, baseline <id>",
     environment=<existing environment with the reqflow_state key REMOVED>,
     decisions=[{ decision: "<profile, scope split, NFR scope>", confidence: "confirmed", reason: "<why>" }],
     handoff_notes=[
       { category: "suggestion",
         note: "Requirements done and baselined (<baseline_id>). Plan doc <doc_id>. Next: run /session-loop; consume todo tasks in plan order." },
       { category: "caution", note: "<open req-followup tasks, unresolved findings>" },
       { category: "context", note: "<profile, id prefixes, NFR doc id, gate settings>" }
     ],
     context_pointers=[{ path: "<plan doc path>", reason: "roadmap and task order" }],
     related_task_ids=["<parent task id>"])
   ```

   The flow is finished, so `reqflow_state` is deliberately dropped from the saved environment
   (merged save, other keys preserved). A later `/requirement-loop` then starts clean instead of
   re-entering Phase 6 and creating a second baseline.

5. Tell the user what `/session-loop` will consume and in what form:
   1. the `todo` tasks (requirement-linked, ordered and scheduled);
   2. the plan document (`doc_type: "plan"`);
   3. the requirement / basic-spec / acceptance layer documents;
   4. the baseline snapshot;
   5. `config.toml` (profile, done_guard, lint policy);
   6. the `suggestion` handoff note with the plan doc id and the start instruction.

   `/session-loop` then uses the plan to prioritize (its Step 2), checks `plan_update_needed`
   when it closes tasks (Step 6) and re-runs `auto_schedule` when `schedule_stale` appears (Step 7).

## Preflight checklist usage

At each gate, open `skills/handoff-requirements/references/preflight-checklists.md`, go through
the checkbox list of that gate **one item at a time**, running the stated tool call. Report the
result to the user as a short table (item / pass or fail / evidence). A failed item sends you
back to fix the phase, not forward with a known gap. Mapping:

| Coordinator gate | Checklist section |
|---|---|
| Gate #0 | Gate 0 to 1 |
| Gate #1 | Gate 1 to 2 |
| Gate #2 | Gate 2 to 3 |
| Gate #3 | Gate 3 to 4 |
| Gate #4 | Gate 4 to 5 |
| Gate #5 | Gate 5 (baseline row completed in Phase 6) |

## Rules

- **No phase starts without the previous gate's user approval** (`AskUserQuestion`) and a
  passing preflight checklist. A "No" or "Revise" answer keeps you in the current phase.
- **Never author requirement text for Phases 3-4 yourself.** Workers write, checkers read,
  the reviewer judges; you only orchestrate, verify, and report.
- **Always launch `name: "handoff-task-loop:requirement-execute"`.** Never an inline script.
- **Never fake a completion report.** `passed: false`, a `null` agent report, or a failed
  verification step is reported to the user, not smoothed over.
- **Do not weaken the gate.** Never lower `fail_on`, add `skip_checks`, or waive a finding to
  reach `passed: true` without the user explicitly asking for it.
- **Never swallow discovered issues** — file them (`req-followup` or per `_bug-report-protocol.md`).
- **Save `reqflow_state` at every gate**, so any approval wait can be resumed; **Suspend** is offered at
  every gate and closes the session; a finished flow carries no `reqflow_state`.
- **Resume only pending scopes**: a `registered` scope is never re-drafted, re-checked or re-registered.
- **Ask in batches.** Collect all open questions for a phase and ask once; do not interrogate
  the user one question at a time.
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only.
- **Do not push.** Stop at commit if you commit at all.
