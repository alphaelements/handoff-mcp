# Handoff Task Loop — Claude Code Plugin

Automated task consumption loop that processes handoff tasks using parallel TDD
implementation, adversarial testing, and architectural review.

**Language and framework agnostic.** The loop handles orchestration; your project's
`CLAUDE.md` provides the build commands, test commands, and coding conventions.

## Prerequisites

- [handoff-mcp](../plugin/) plugin installed and enabled
- `handoff-mcp` binary installed (`npm i -g handoff-mcp-server` or `cargo install handoff-mcp`)
- A project initialized with handoff (`handoff_init`)
- Project-specific build/test/lint commands documented in your project's `CLAUDE.md`

## What's included

### Development loop
- **Agents** — session-developer (Sonnet), session-tester (Sonnet),
  session-integration-tester (Sonnet), session-reviewer (Opus),
  session-spec-writer (Sonnet — Stage 0: derives missing basic_spec from requirements, scaffolds system_test (full: + detailed_spec/unit_test), self-checks with confidence + checker_result),
  session-spec-reconciler (Sonnet — Stage 3.5: syncs basic_spec/system_test with the implementation, reports contract_ambiguity),
  session-closer (Sonnet — automates Step 6 close-out: done_criteria, status, trace_update),
  session-closer-verifier (Sonnet, read-only — reads each task back from disk after the closer so the workflow can compare status and checked criteria with the closer's claims),
  elite-developer (Fable — the single agent of the `elite` profile: implements with TDD, tests its own work adversarially, runs the full suite, reviews and fixes in one context, and reports seven stages per task; reads handoff state and never writes it. `session-execute` launches exactly one per round under `profile: 'elite'`, in place of the developers, the tester, and the reviewer),
  requirement-worker (Sonnet — decomposes one scope into a requirement-layer document with acceptance criteria),
  requirement-checker (Sonnet) and requirement-reviewer (Opus) — read-only requirement quality gates (per-document 29148/NFR/completeness check, plus the opt-in ISO 29119 `test_spec` check of scaffolded test specifications; cross-document duplicate/contradiction/omission review),
  requirement-registrar (Sonnet — registers finished requirement docs: trace_scaffold, trace_tasks, lint verification)
- **Workflow** — `session-execute` (parallel implement -> test -> review -> close, with owner-limited rework)
- **Command** — `/session-loop` (session manager orchestrator)
- **Protocol** — `_bug-report-protocol` (discovered issue tracking)

### Research loop
- **Agents** — research-investigator (Sonnet), research-verifier (Sonnet), research-drafter (Sonnet), research-director (Opus),
  elite-researcher (Fable — the single agent of the `elite` profile: investigates every facet, tries to refute its own findings, drafts the document, and judges both gates in one context; a finding its verification refuted stays out of the draft. `research-execute` launches exactly one under `profile: 'elite'`, in place of the investigators, verifiers, drafter and director)
- **Workflow** — `research-execute` (investigate -> verify -> gate -> draft -> review; under `profile: 'elite'` one elite-researcher runs the whole cycle and its report is mapped onto the same result shape)
- **Command** — `/research-loop` (research coordinator)

### Brainstorm research
- **Agents** — brainstorm-regulatory-director (Opus — derives the legal/regulatory scope from the deliverable's characteristics, assigns investigators, integrates and verifies their results), brainstorm-regulatory-investigator (Sonnet — researches one regulatory area on the web), brainstorm-adversarial-reviewer (Opus — reviews all technical and regulatory results together and rates risks BLOCKER/MAJOR/MINOR with mitigations),
  elite-investigator (Fable — the single agent of the `elite` profile: covers the technical investigation and the regulatory research in one context and attacks its own findings with a risk-rated self-review; reads handoff state and never writes it. `brainstorm-execute` launches exactly one under `profile: 'elite'`, in place of the technical investigators, the regulatory director and its investigators; the adversarial reviewer still runs as its own agent)
- Technical research reuses research-investigator and research-verifier
- **Workflow** — `brainstorm-execute` (Stream A: parallel technical investigation; Stream B: regulatory director -> parallel investigators -> integration; then adversarial review of both; under `profile: 'elite'` one elite-investigator covers both streams and its report is mapped onto the same result shape). Launched through the `brainstorm-execute` skill, never as an inline script
- **Skill** — `brainstorm-execute` (how the coordinator builds the workflow args, reads the result and files findings into report sections 6.1-6.4)
- **Command** — `/brainstorm-loop` (brainstorm coordinator; Phases 0-1 and 3-4 dialogue with proposal-style facilitation and a hidden coverage tracker, Phase 2 research, delegate mode; recommends `/spec-loop` next (optional) and hands the finalized report to `/requirement-loop` either way)

### Requirement loop
- **Agents** — requirement-worker (Sonnet), requirement-checker (Sonnet), requirement-reviewer (Opus), requirement-registrar (Sonnet), test-spec-worker (Sonnet — completes the `(TBD)` preconditions / steps / environment of scaffold-generated ISO 29119 test items; marks guesses `(要確認)`; never changes `method` or `priority`),
  elite-requirement-author (Fable — the single agent of the `elite` profile: decomposes every scope, checks each document against ISO/IEC/IEEE 29148, reviews them across documents, fixes what it finds within `max_rounds`, and registers them in one context. `requirement-execute` launches exactly one under `profile: 'elite'`, in place of the workers, checkers, reviewer, registrar and test-spec-worker)
- **Workflow** — `requirement-execute` (draft per scope -> check per document -> cross-document review -> register tasks and verification links; under `profile: 'elite'` one elite-requirement-author runs all four stages and its report is mapped onto the same result shape)
- **Command** — `/requirement-loop` (requirements coordinator; Phase 0 imports the approved `/spec-loop` specification when one exists, else the brainstorm report or manual input; Phases 0-2 elicitation dialog, Phases 3-4 Workflow, Phases 5-6 close-out)
- **Suspend / resume** — the flow can be stopped at any gate; its state (`reqflow_state`) is saved in the session `environment` and a later `/requirement-loop` resumes from the saved phase, re-running only scopes that are not yet registered

### Spec loop
- **Agents** — spec-architect (Sonnet — Phase 1: Mermaid `flowchart` / `C4Context` architecture), spec-gui-designer (Sonnet — Phase 2: one HTML mock per screen and a `stateDiagram-v2` of the transitions), spec-usecase-analyst (Sonnet — Phase 3: use cases with `sequenceDiagram`),
  elite-spec-author (Fable — the single agent of the `elite` profile: designs the architecture, the GUI mocks with their transition diagram and the use cases in one context and drafts the six-section specification; read-only towards handoff, writes only the mocks. `spec-execute` launches exactly one, in place of the three phase agents)
- **Skill** — `spec-execute` (how the coordinator briefs the phase agents, validates Mermaid, assembles, saves and exports the specification; section 7 covers the elite run)
- **Workflow** — `spec-execute` (elite only: one elite-spec-author generates all three areas and the draft, and the result is checked before the coordinator's gates; the other profiles generate phase by phase through the skill and have no workflow run)
- **Command** — `/spec-loop` (specification coordinator between `/brainstorm-loop` and `/requirement-loop`; Phase 0 import and scope gate, Phases 1-3 generation with a user gate each, Phase 4 document, PDF and final approval)

### GUI verification
- **Skill** — `gui-vrt` (mock -> golden -> verify pipeline; modes `capture-golden`, `verify`, `compare`, `ai-compare`; `verify` runs on the MCP chain `doc_req_list` -> `trace_test_run` -> `trace_record`, recording pass/fail with golden / `vrt-actual` / `vrt-diff` evidence and `VrtEvidence` numbers)
- **Agent** — gui-visual-verifier (Sonnet — read-only AI visual comparison of two screenshots for `ai-compare`)
- **Setup** — `skills/gui-vrt/references/setup-guide.md` (Playwright, pixelmatch, code-server, CI with xvfb, `[gui]` config) and the `vrt-compare.mjs` pixel comparison helper
- Golden storage and the `mock` attribute are managed by hand for now; server-side support (`golden`/`diff` evidence validation, `mock` parsing) is planned separately

## Architecture

### Development loop

```
Session Manager (main agent, /loop /session-loop)
 |-- Fetches tasks from handoff
 |-- Splits into sessions (1-5 tasks each)
 |-- Picks a pipeline profile, gets user approval
 |-- Launches Workflow(session-execute)
 |    |
 |    |-- Stage 0: Spec-Prep — spec-writer derives missing basic_spec + scaffolds (Sonnet) [standard, full]
 |    |-- Main loop (up to max_rounds, one flat loop, 3 serial stages):
 |    |   |-- Stage 1: Parallel developers (TDD, Sonnet)              [every profile]
 |    |   |-- Stage 2: Single tester — per-task adversarial verification
 |    |   |            + whole-project suite / E2E / wiring (Sonnet)  [standard, full]
 |    |   |-- Stage 3: Single reviewer — design / test-code quality (Opus) [full only]
 |    |   |
 |    |   Round 1 must surface every defect it can see — no basis creep across
 |    |   rounds. FAIL / REQUEST_CHANGES, round < max_rounds -> rework -> Stage 1.
 |    |   FAIL / REQUEST_CHANGES on the LAST round -> no escalation: the session
 |    |   still completes (passed: true), and whatever is unresolved is returned
 |    |   as `pending_followups` for the manager to file as backlog tasks.
 |    |
 |    |   Under `elite`, Stages 1-3 are ONE elite-developer (Fable) per round,
 |    |   with the same rework and no-escalation rules (see Profiles).
 |    |
 |-- Processes results, marks tasks done, files pending_followups as tasks,
 |   commits
 +-- Hands off to next session
```

### Document-aware agents

All agents query project documents (specs, designs, ADRs) via `handoff_doc_query`:

- **Developer**: Calls `handoff_doc_query(task_id=...)` at task start to surface linked specs. Uses spec sections as an implementation guide.
- **Tester**: Cross-checks test coverage against spec sections. Gaps between spec and tests are reported as findings.
- **Reviewer**: Calls `handoff_doc_verify_status` to check spec alignment. Unverified spec sections are flagged as BLOCKERs.
- **Manager**: Calls `handoff_task_checklist(action="view")` during planning to show readiness baseline and uncovered sections.

### Four verification layers

Each layer is defined by **what only it can see**:

| Layer | Scope | Answers |
| --- | --- | --- |
| developer | its own tasks | does my change work? (red → green) |
| tester | its own tasks | what does this test suite fail to guarantee? |
| integration tester | the whole tree, once | is it wired? does the whole suite and E2E pass? |
| reviewer | everything, incl. test code | is the design right? is the test code itself correct? |

The tester deliberately does **not** run the whole suite, run E2E, or judge wiring — while it
runs, another work group may still be implementing, so any whole-tree verdict would be a
verdict on a half-built tree. It also does not re-run what the developer already ran green;
that yields no information. Instead it attacks the tests: do they execute, do they go red when
the implementation is broken, would they have passed against the *old* code, and what does no
test cover? The integration tester then checks the assembled system exactly once — catching
the defect where every unit test is green and the feature does not work because nothing calls it.

### Profiles

The **profile** selects how many serial agent turns a session costs — the
dominant term in wall-clock latency:

| Profile | Stages | Serial turns |
| --- | --- | --- |
| `express` | developer | 1 |
| `standard` *(default)* | spec-prep → developer → tester → spec-reconcile | 4 |
| `full` | spec-prep → developer → tester → reviewer → spec-reconcile | 5 |
| `elite` *(opt-in)* | spec-prep → elite-developer → spec-reconcile | 3 |

Under `elite` one `elite-developer` (Fable) does the work of the developers, the tester, and the
reviewer for every task, in a single context, and returns one report with a
`## dev result: <task_id>` block per task. The workflow cuts that report into one developer report
per task and reads the suite and review verdicts from it, so Spec-Reconcile and the closer run as
they do under `full`, as separate Sonnet agents. If the report leaves findings open, the agent is
launched again with them, up to `max_rounds` launches; what is still open then is returned as
`pending_followups`. A task with no block of its own fails the session, like a developer that
never reported. `dev_assignments` is not required under `elite`, and `budgets` are not applied.

The integration tester handles both per-task adversarial checks and whole-project
suite / E2E / wiring in a single agent invocation, so `test` and `integrate` share
one serial turn. `full` adds the reviewer as an extra turn; `standard`/`full` also pay one turn each for Spec-Prep (Stage 0) and Spec-Reconcile (Stage 3.5). `express` has no
integration tester — its tasks are mechanical and self-verifying, so there is no
wiring to check, and the developer owns the whole-project suite there.

Developers run format, lint, and type check under **every** profile, plus the tests in their
own scope. `express` drops the adversarial layers, not the gates. `/session-loop` picks a
profile from task estimates and labels and confirms it with you. See its step 2b for the rules.

You can choose the profile yourself instead. For one run, name it in the invocation
(`/session-loop t1,t2 profile: elite`). As the project default, set it in `.handoff/config.toml`
(or with `handoff_update_config(updates: {"session_loop.profile": "elite"})`, which rejects an
unknown profile name):

```toml
[session_loop]
profile = "elite"
```

A profile named for one run wins over the configured one, and the configured one wins over the
rules. With neither set, nothing changes: the rules choose `express`, `standard` or `full`.
`elite` is never chosen for you — it runs only when you ask for it, and `/session-loop` shows a
cost warning (it uses the Fable model) before each session that runs under it.

`integration_expected` (default `true`) controls the wiring verdict. Set it to `false` when a
session deliberately builds a foundation to be wired later — the whole suite and E2E still run
and must still pass; only the wiring check is suspended.

The profile also sets **reasoning effort**: the `express` developer runs at `medium`,
every other agent at `high`. The tester, integration tester, and reviewer are the adversarial
layers, so a session that pays for them never makes them think less.

Session context is fetched **once** by the manager and injected into every agent's prompt
— no agent calls `handoff_load_context` for bytes the manager has already read. Agents
still fetch what depends on their own work: `handoff_get_task`, `handoff_memory_query`,
and — reviewer only — `handoff_list_tasks`. The reviewer has write access during escalation.

### Research loop

```
Research Coordinator (main agent, /loop /research-loop)
 |-- Decomposes topic into facets
 |-- Assigns investigators and verifiers
 |-- Gets user approval
 |-- Launches Workflow(research-execute)
 |    |
 |    |-- Investigation loop (up to 2 rounds):
 |    |   |-- Phase 1: Parallel investigators (Sonnet xN)
 |    |   +-- Phase 2: Parallel verifiers (Sonnet xN, adversarial)
 |    |   +-- Phase 3: Director gate (Opus x1)
 |    |   (REINVESTIGATE -> loop with narrowed gaps)
 |    |
 |    |-- Document loop (up to 2 rounds):
 |    |   +-- Phase 4: Drafter (Sonnet x1)
 |    |   +-- Phase 5: Director review (Opus x1)
 |    |   (REVISE -> loop with specific instructions)
 |    |
 |-- Saves document, updates handoff
 +-- Closes session
```

Investigators explore facets in parallel; verifiers cross-check adversarially.
The director (Opus) gates transitions — only verified findings reach the drafter.

## Installation

```bash
# 1. Install handoff-mcp binary (required)
npm i -g handoff-mcp-server        # or: cargo install handoff-mcp

# 2. Install the base plugin + task loop from GitHub Marketplace
/install alphaelements/handoff-mcp
```

After running `/install`, both `handoff-mcp` (base) and `handoff-task-loop` are available.
Enable the task loop:

```
/plugin enable handoff-task-loop
```

## Getting started

After installation:

```
# Start the loop (processes all todo tasks)
/loop /session-loop

# Process specific tasks
/loop /session-loop t1,t2,t3

# Process a range
/loop /session-loop t5-t9

# Process from t5 onward
/loop /session-loop t5-

# Natural language stop condition
/loop /session-loop goal: all P1 tasks complete

# Research / specification workflow
/research-loop RP2350 ADC noise characteristics output: spec
/research-loop MCP protocol error handling output: report path: wiki/42-errors.md

# Requirements authoring workflow (resumes automatically if a suspended flow exists)
/requirement-loop
```

## How it works with your project

The task loop **does not hardcode any language or framework**. Instead:

1. **Agents read your `CLAUDE.md`** at runtime for build, test, lint, and formatting commands
2. **Agents follow your `CLAUDE.md`** coding conventions (whatever they are)
3. **The workflow orchestrates** the TDD cycle, adversarial testing, and review — this is language-agnostic
4. **Handoff manages state** — tasks, sessions, criteria checklists, and inter-session context

### Required CLAUDE.md sections

The task loop agents look for the following in your project's `CLAUDE.md`.
Copy this template and fill in your project's specifics:

```markdown
## Build & Test

# Commands the agents will run (fill in yours)
<install command>        # e.g. npm install / pip install -e . / cargo build
<build command>          # e.g. npm run build / make / cargo build --release
<test command>           # e.g. npm test / pytest / cargo test
<lint command>           # e.g. npm run lint / ruff check . / cargo clippy -- -D warnings
<format command>         # e.g. npm run format / black . / cargo fmt
<typecheck command>      # e.g. npx tsc --noEmit / mypy . / (included in cargo build)

## Coding Rules

- <language mode>        # e.g. TypeScript strict / Python 3.12+ / Rust edition 2021
- <linter/formatter>     # e.g. ESLint + Prettier / ruff + black / rustfmt + clippy
- <key conventions>      # e.g. no `any` / no print() in production / #![deny(warnings)]

## Project Structure

<brief description of architectural layers and key directories>
# Agents use this to understand where code belongs and avoid file conflicts

## Session Handoff

This project uses handoff-mcp for session continuity.

- **Session start**: Call `handoff_load_context`
- **Session end**: Call `handoff_save_context` with summary, decisions, blockers
- **During work**: Use `handoff_update_task` to track progress
- **Spec registration**: When writing a spec/design, call `handoff_doc_save(task_ids=[...])`
  to link it to tasks. Generate a verification matrix with `handoff_doc_verify(action="generate")`.
- **Check readiness**: `handoff_task_checklist(task_id=..., action="view")` for combined
  done_criteria + verification coverage.

## Documentation Contract

# Source-of-truth documents and update rules for session-doc-reconciler.
# List each canonical document, its path or HandoffDoc slug, and when to update it.

# Path/glob → document mapping:
#   src/api/**     → wiki/api-reference.md       (generated: `npm run gen:api-docs`)
#   CHANGELOG.md   → manual, user-facing only
#   <spec-slug>    → HandoffDoc (update via `handoff_doc_update_section`, never edit .handoff/ directly)

# Rules:
# - Only update a normative spec when there is evidence of an intended contract change
#   (task requirement, user instruction, or session-approved decision).
# - Code-spec contradictions without intent evidence → code finding, not spec update.
# - For generated docs, run the generator/validator command, never hand-edit the output.
# - HandoffDoc verification matrix: `handoff_doc_verify(action="sync")` after updates,
#   target stale=0 before marking the task done.
# - When doc_target is ambiguous or unknown, do not guess — leave it for closure review.
```

### AGENTS.md (for Codex / non-Claude agents)

If your project also uses Codex or other AI coding agents, maintain a separate
`AGENTS.md` with the same documentation contract adapted to that agent's
capabilities:

```markdown
## Documentation

- Docs location: see `CLAUDE.md` → Documentation Contract for path mapping.
- Do NOT edit `.handoff/` files directly — use Handoff MCP tools (`handoff_doc_save`,
  `handoff_doc_update_section`, `handoff_doc_verify`).
- Claude Code assets (`.claude/`, plugin configs) and Codex assets (`codex.md`,
  `.codex/`) are separate — do not cross-edit.
- When updating a spec, verify intent evidence exists (task, user instruction, or
  approved decision) before changing normative content.
```

### What agents need at minimum

| Section | Used by | Purpose |
|---|---|---|
| Build & Test | Developer, Tester, Integration tester | TDD in scope; whole-suite, lint, and E2E commands for the integration pass |
| Coding Rules | Developer, Tester, Integration tester | Enforce project conventions, catch violations |
| Project Structure | Manager, Reviewer, Integration tester | Assign tasks without file conflicts; review architecture; trace wiring across layers |
| Session Handoff | Manager, Developer, Reviewer | Establish sessions, track progress, register specs, check readiness |
| Documentation Contract | Doc reconciler, Reviewer | Resolve doc targets, enforce update rules, prevent spec laundering |

If any section is missing, agents will ask you or make best-effort guesses — but
explicit documentation produces much better results.

## Configuration

Model selection and loop behavior are tuned per session through the workflow's
`args` (see `/session-loop` step 5):

| `args` field               | Default    | Description                                            |
| -------------------------- | ---------- | ------------------------------------------------------ |
| `profile`                  | `standard` | Pipeline depth: `express` / `standard` / `full` / `elite` (see Profiles for how `/session-loop` chooses it) |
| `integration_expected`     | `true`     | Must the session's work be wired into the system?       |
| `dev_model`                | `sonnet`   | Model for developers (not used under `elite`)           |
| `elite_model`              | `fable`    | Model for the elite-developer (`elite` only; omit unless the user names a model) |
| `tester_model`             | `sonnet`   | Model for testers (unused under `express`)              |
| `integration_tester_model` | `sonnet`   | Model for the integration tester (unused under `express`) |
| `reviewer_model`           | `opus`     | Model for the reviewer (only runs under `full`)         |
| `max_rounds`               | `3`        | Max implement/test rounds (`express` always runs 1; under `elite`: max elite-developer launches, and the limit of its own fix loop) |
| `max_review_rounds`        | `2`        | Max rework rounds after the verify stage fails          |

Both round budgets must be positive integers when given; `0`, a negative value,
or a non-number is rejected rather than silently coerced. Omit them for the
defaults. `integration_expected` must be a real boolean — the string `'false'` is
truthy and is rejected rather than silently enabling the check it meant to disable.

Per-assignment `model_override` takes priority over the model defaults.
The manager caps a session at 5 tasks.

### Research loop (`/research-loop`)

| Parameter                  | Default  | Description                               |
| -------------------------- | -------- | ----------------------------------------- |
| `INVESTIGATOR_MODEL`       | `sonnet` | Model for investigators                   |
| `VERIFIER_MODEL`           | `sonnet` | Model for verifiers                       |
| `DRAFTER_MODEL`            | `sonnet` | Model for drafter                         |
| `DIRECTOR_MODEL`           | `opus`   | Model for director (gate + review)        |
| `MAX_INVESTIGATION_ROUNDS` | `2`      | Max investigation/re-investigation rounds |
| `MAX_REVISION_ROUNDS`      | `2`      | Max draft revision rounds                 |
| `PROFILE`                  | `standard` | `elite` = one elite-researcher runs the whole cycle; every other profile runs the multi-agent pipeline |
| `ELITE_MODEL`              | `fable`  | Model for the elite-researcher (`elite` only; omit unless the user names a model) |

## Safety

- **Quality gates**: Tester FAIL or reviewer REQUEST_CHANGES triggers owner-limited rework
  (up to 3 rounds). Only the developer(s) whose tasks have findings are re-launched; previous
  reports from unaffected developers are preserved. Unresolved findings on the last round
  are filed as follow-up tasks — never silently dropped, and the session still completes.
- **Fail-closed everywhere**: A crashed agent returns `null`, which is never read as a pass.
  A dead integration tester found no wiring defect — that is not the same as there being none.
  An unwired implementation fails even when every unit test is green.
- **Agent context**: The manager fetches session context once and injects it into every
  agent, so no agent re-reads it over MCP. Agents keep read access to what depends on their
  own work (`handoff_get_task`, `handoff_memory_query`; reviewer also `handoff_list_tasks`).
  Only the reviewer has write access, and only during escalation.
- **Honest reporting**: All agents are instructed to report failures truthfully
- **No push**: Stops at commit — pushing requires explicit user approval
- **Handoff-only**: `.handoff/` direct editing is forbidden
- **User approval**: Session plans and uncertainties are confirmed before implementation starts

## Links

- [handoff-mcp](https://github.com/alphaelements/handoff-mcp)
