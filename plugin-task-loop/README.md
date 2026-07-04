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

- **Agents** — session-developer (Sonnet), session-tester (Sonnet), session-reviewer (Opus)
- **Workflow** — `session-execute` (parallel implement -> test -> review with rework loop)
- **Command** — `/session-loop` (session manager orchestrator)
- **Protocol** — `_bug-report-protocol` (discovered issue tracking)

## Architecture

```
Session Manager (main agent, /loop /session-loop)
 |-- Fetches tasks from handoff
 |-- Splits into sessions (1-5 tasks each)
 |-- Gets user approval
 |-- Launches Workflow(session-execute)
 |    |
 |    |-- Inner loop (up to 3 rounds):
 |    |   |-- Phase 1: Parallel developers (TDD, Sonnet)
 |    |   +-- Phase 2: Parallel testers (adversarial, Sonnet)
 |    |   (test FAIL -> rework, repeat inner loop)
 |    |
 |    |-- Final Review (1x after tests pass):
 |    |   +-- Single reviewer (architecture, Opus)
 |    |   APPROVE -> done
 |    |   REQUEST_CHANGES -> Review rework (max 2 rounds):
 |    |     Implement -> Test -> Re-review
 |    |     (escalate to handoff if still failing)
 |    |
 |-- Processes results, marks tasks done, commits
 +-- Hands off to next session
```

Agents have read access to handoff context (previous session decisions, project memory)
for better cross-session awareness. The reviewer has write access during escalation.

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
```

### What agents need at minimum

| Section | Used by | Purpose |
|---|---|---|
| Build & Test | Developer, Tester | Know which commands to run for TDD and verification |
| Coding Rules | Developer, Tester | Enforce project conventions, catch violations |
| Project Structure | Manager, Reviewer | Assign tasks without file conflicts, review architecture |
| Session Handoff | Manager | Establish and close sessions correctly |

If any section is missing, agents will ask you or make best-effort guesses — but
explicit documentation produces much better results.

## Configuration

Model selection and loop behavior can be tuned per session via the manager:

| Parameter               | Default  | Description                                  |
| ----------------------- | -------- | -------------------------------------------- |
| `DEV_MODEL`             | `sonnet` | Model for developers                         |
| `TESTER_MODEL`          | `sonnet` | Model for testers                            |
| `REVIEWER_MODEL`        | `opus`   | Model for reviewer                           |
| `MAX_TASKS_PER_SESSION` | `5`      | Max tasks per session                        |
| `MAX_REWORK_ROUNDS`     | `3`      | Max test-level rework rounds                 |
| `MAX_REVIEW_ROUNDS`     | `2`      | Max review rework rounds after final review  |

## Safety

- **Quality gates**: Tester FAIL triggers inner rework loop (up to 3 rounds). After tests pass,
  Reviewer REQUEST_CHANGES triggers review rework (up to 2 rounds). Unresolved review issues
  are escalated to handoff for the next session — never silently dropped.
- **Agent context**: Agents have read access to handoff context (previous session decisions,
  project memory) for better cross-session awareness. Only the reviewer has write access,
  and only during escalation.
- **Honest reporting**: All agents are instructed to report failures truthfully
- **No push**: Stops at commit — pushing requires explicit user approval
- **Handoff-only**: `.handoff/` direct editing is forbidden
- **User approval**: Session plans and uncertainties are confirmed before implementation starts

## Links

- [handoff-mcp](https://github.com/alphaelements/handoff-mcp)
