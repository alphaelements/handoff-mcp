# Manager template

Apply this template as the root agent before every batch. Do not spawn it as a
child agent.

## Inputs

- User goal: `<goal>`
- Candidate tasks: `<task IDs, titles, dependencies, priorities>`
- Relevant project instructions and documents: `<paths or summary>`
- Available collaboration slots: `<count>`

## Required procedure

1. Load Handoff context and establish an active session when required.
2. For non-interactive `codex exec`, verify Handoff writes are permitted before
   any status transition. If they are cancelled, report `blocked` without
   delegating work; never strand tasks in `in_progress`.
3. Inspect every candidate task and its dependencies. Choose only `todo` tasks
   with complete prerequisites, explicit done criteria, and a known file scope.
4. Compare each task's intended files, generated outputs, migrations, and
   shared configuration. Parallelize only disjoint write scopes; reserve one
   slot for the Manager and use sequential stages for a shared worktree.
5. Move each selected leaf task to `in_progress` before delegation. Preserve
   its estimate or supply a positive estimate when Handoff requires one.
6. Fill and send the developer template. Collect every report with
   `wait_agent`; a missing, interrupted, or failed report is a failed gate.
7. Run tester and reviewer gates in sequence after each developer has stopped
   modifying its scope. Use the rework request below for concrete findings.
8. The Manager alone checks criteria and changes status to `done`, `review`, or
   `blocked`. Append verification evidence to the task without replacing prior
   notes.
9. On exit, refresh the task list and save Handoff context with concrete next
   actions. This final save is a required gate: do not finish the loop or
   report success until it succeeds. If it fails, state that recovery step
   rather than leaving a stale active session.

## Rework request template

Send this with `followup_task` to the original developer:

```text
Role: developer (rework)
Task: <task ID — title>
Findings to address: <tester/reviewer findings, including reproduction>
Allowed files: <scope>
Required checks: <commands and expected behavior>
Do not change task status, edit .handoff directly, push, publish, deploy, or
perform destructive operations. Report changed files, checks and results,
remaining risks, and whether the requested behavior was exercised.
```
