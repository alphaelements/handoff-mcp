---
name: handoff-session-loop
description: Run explicitly selected, ready Handoff tasks through a safe Codex-native developer, tester, and reviewer loop. Use when the user invokes `$handoff-session-loop` or asks to run a Handoff task loop in Codex.
---

# Handoff Session Loop

Use this skill only after an explicit user request. It is a Codex-native
orchestrator: do not invoke Claude Code commands, Claude workflows, or files
from `plugin-task-loop/`.

Read `references/manager.md` before selecting work. Before each delegation,
read the matching role template in `references/` and replace every
`<placeholder>` with the task-specific facts. The templates define the
required inputs and report format; the Manager remains accountable for all
Handoff state changes.

## Prerequisites and safety

1. Confirm that the Handoff MCP tools are available. If they are not, explain
   that the project must first configure Handoff MCP; do not emulate its task
   state by editing `.handoff/` files.
2. In non-interactive `codex exec`, confirm that Handoff write calls are
   explicitly permitted before selecting work. Set
   `mcp_servers.handoff.default_tools_approval_mode = "auto"` for the
   controlled invocation, or use the interactive CLI. If Handoff write calls
   are cancelled, do not delegate work or leave tasks `in_progress`; report the
   configuration blocker instead.
3. Work only in the current project. Never push, publish, deploy, run a
   migration, or perform a destructive operation without the user's explicit
   approval.
4. The root agent is the Manager. It retains responsibility for task state and
   final verification; child agents must not mark a task done merely because
   their local work appears complete.
5. Treat every child-agent failure, timeout, or missing report as a failed
   gate, never as a pass.

## Start and select work

1. Call `handoff_load_context` for the current project. If it asks for an
   active session, immediately call `handoff_save_context` with
   `session_status: "active"`, inherited context, a concrete next action, and
   a checklist item.
2. Call `handoff_list_tasks`. Select only tasks that are `todo`, have no
   unfinished dependencies, and have a clear, independently writable file
   scope. Inspect each candidate with `handoff_get_task` before choosing it.
3. If scope, acceptance criteria, required tooling, or file ownership is
   unclear, leave the task `todo` or set it to `review` and ask the user for
   direction. Do not invent requirements.
4. Form small batches. Concurrent developer work is permitted only when the
   selected tasks do not share files or generated outputs. Keep one available
   collaboration slot for the Manager; otherwise run work sequentially.
5. Before delegating a leaf task, update it to `in_progress` with
   `handoff_update_task`. Supply a positive `schedule.estimate_hours` when
   required by Handoff.

## Developer stage

For each non-conflicting task, read `references/developer.md`, fill its task
context, and use it as the `collaboration.spawn_agent` message. Do not start a
developer with incomplete scope, criteria, or safety constraints.

Wait for every developer report with `wait_agent`. If a developer discovers a
safe, specific repair, use `followup_task` to request it. If it cannot proceed,
record the reason and set the task `blocked` or `review` through Handoff.

## Verification stages

Process each completed developer task sequentially through the following
gates, so agents do not inspect a moving shared worktree.

1. Spawn a **tester** child agent. Give it the task ID, done criteria, files
   changed by the developer, and the developer's reported commands by filling
   `references/tester.md`. It must not declare a task done.
2. When the tester passes, spawn a **reviewer** child agent. Give it the same
   task context plus the tester report by filling `references/reviewer.md`.
3. If either gate finds an issue, send the concrete findings to the original
   developer with `followup_task`, then repeat the affected verification gate.
   Bound rework attempts and surface unresolved issues as `review` or
   `blocked`; do not silently accept them.

## Complete task state

Only the Manager may transition a task to `done`:

1. Confirm every done criterion against the implementation, automated checks,
   and a real execution when the artifact has one.
2. Call `handoff_check_criterion` as each criterion becomes true.
3. Make one final `handoff_update_task` call with status `done` only after all
   criteria are checked. Use `review` when user confirmation is required and
   `blocked` when an external condition prevents progress.
4. Add concise task notes with the implemented behavior, verification command,
   and any known limitation. Do not replace existing notes unnecessarily.

## End or resume the loop

At the end of a batch, call `handoff_list_tasks` again. Save session context
with `handoff_save_context`: a one-sentence summary, decisions, blockers,
context pointers, and 2–3 concrete next actions. Keep the session active only
when the loop is continuing in the same conversation; otherwise close it.

This final save is mandatory after any task-state transition. Do not return a
final result until it succeeds. If saving fails, keep the affected tasks out of
`done` when possible, record the failure as `review` or `blocked`, and report
the recovery step explicitly.

On a later invocation, begin again from `handoff_load_context` and the stored
suggestions. Do not rerun verification that a previous session recorded as
complete unless relevant files have changed.
