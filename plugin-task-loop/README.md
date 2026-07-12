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
  session-integration-tester (Sonnet), session-reviewer (Opus)
- **Workflow** — `session-execute` (parallel implement -> scoped test -> integrate ∥ review, with rework loop)
- **Command** — `/session-loop` (session manager orchestrator)
- **Protocol** — `_bug-report-protocol` (discovered issue tracking)

### Research loop
- **Agents** — research-investigator (Sonnet), research-verifier (Sonnet), research-drafter (Sonnet), research-director (Opus)
- **Workflow** — `research-execute` (investigate -> verify -> gate -> draft -> review)
- **Command** — `/research-loop` (research coordinator)

## Architecture

### Development loop

```
Session Manager (main agent, /loop /session-loop)
 |-- Fetches tasks from handoff
 |-- Splits into sessions (1-5 tasks each)
 |-- Picks a pipeline profile, gets user approval
 |-- Launches Workflow(session-execute)
 |    |
 |    |-- Inner loop (up to 3 rounds), per independent work group:
 |    |   |-- Phase 1: Parallel developers (TDD, Sonnet)      [every profile]
 |    |   +-- Phase 2: Parallel testers (adversarial, Sonnet) [standard, full]
 |    |   (each verifies ONLY its own scope; test FAIL -> rework, repeat)
 |    |
 |    |-- Verify stage (1x after the loop converges) — both run CONCURRENTLY:
 |    |   |-- Integration tester (Sonnet)  whole suite / E2E / wiring [standard, full]
 |    |   +-- Reviewer (Opus)              design / test-code quality  [full only]
 |    |   BOTH pass -> done
 |    |   EITHER fails -> Rework (max 2 rounds):
 |    |     Implement -> Test -> Re-verify
 |    |     (escalate to handoff if still failing)
 |    |
 |-- Processes results, marks tasks done, commits
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
| `standard` *(default)* | developer → tester → integrate | 3 |
| `full` | developer → tester → (integrate ∥ review) | 3 |

`full` costs the same as `standard`: the integration tester and the reviewer run in one
parallel barrier, so the reviewer is free in wall-clock terms. `express` has no integration
tester — its tasks are mechanical and self-verifying, so there is no wiring to check, and the
developer owns the whole-project suite there.

Developers run format, lint, and type check under **every** profile, plus the tests in their
own scope. `express` drops the adversarial layers, not the gates. `/session-loop` picks a
profile from task estimates and labels and confirms it with you. See its step 2b for the rules.

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
```

### What agents need at minimum

| Section | Used by | Purpose |
|---|---|---|
| Build & Test | Developer, Tester, Integration tester | TDD in scope; whole-suite, lint, and E2E commands for the integration pass |
| Coding Rules | Developer, Tester, Integration tester | Enforce project conventions, catch violations |
| Project Structure | Manager, Reviewer, Integration tester | Assign tasks without file conflicts; review architecture; trace wiring across layers |
| Session Handoff | Manager, Developer, Reviewer | Establish sessions, track progress, register specs, check readiness |

If any section is missing, agents will ask you or make best-effort guesses — but
explicit documentation produces much better results.

## Configuration

Model selection and loop behavior are tuned per session through the workflow's
`args` (see `/session-loop` step 5):

| `args` field               | Default    | Description                                            |
| -------------------------- | ---------- | ------------------------------------------------------ |
| `profile`                  | `standard` | Pipeline depth: `express` / `standard` / `full`         |
| `integration_expected`     | `true`     | Must the session's work be wired into the system?       |
| `dev_model`                | `sonnet`   | Model for developers                                    |
| `tester_model`             | `sonnet`   | Model for testers (unused under `express`)              |
| `integration_tester_model` | `sonnet`   | Model for the integration tester (unused under `express`) |
| `reviewer_model`           | `opus`     | Model for the reviewer (only runs under `full`)         |
| `max_rounds`               | `3`        | Max implement/test rounds (`express` always runs 1)     |
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

## Safety

- **Quality gates**: Tester FAIL triggers the inner rework loop (up to 3 rounds). After the
  loop converges, the verify stage runs — a FAIL from **either** the integration tester or
  the reviewer triggers rework (up to 2 rounds), and both sets of findings reach the developer
  together. Unresolved issues are escalated to handoff for the next session — never silently
  dropped.
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
