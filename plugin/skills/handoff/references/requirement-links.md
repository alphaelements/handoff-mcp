# Requirement links on tasks

In a project that uses V-model traceability (it has at least one layer
document, see the `handoff-trace` skill), every task is expected to trace back
to a requirement. This page covers what the task tools say about a task that
does not, and what to do about it.

## On creation

In a project that has at least one layer
document, creating a task without `requirement_ids` still succeeds, and the
response ends with two information lines:

```
Info [REQUIREMENT_LINK_MISSING]: Task t12 has no requirement link. Link it to an existing requirement with handoff_update_task(task: {id: "t12", requirement_ids: [...]}), or add the missing requirement first; label the task "no-trace" if it is deliberately outside requirement traceability.
Info [TRACE_PROPOSE]: handoff_trace_propose(task_id: "t12") lists existing requirements that may already cover this task and drafts a new one when none does.
```

Act on them before starting the work: run `handoff_trace_propose`, then link
the task to a candidate, or write the missing requirement first (a new feature
means a requirement was missing; a bug fix belongs to the requirement it
violates). Use the `no-trace` label only for work with no requirement behind
it, such as housekeeping. The lines are never added when an existing task is
updated, when the new task carries `requirement_ids` or the `no-trace` label,
or in a project with no layer document. `handoff_import_context` reports the
same thing once for the whole import, as one `Info [REQUIREMENT_LINK_MISSING]`
line listing the imported tasks that have no link. A task left unlinked is
reported by `handoff_trace_lint` (`task_orphan`) and warned about when it
moves to `review`/`done` (`[trace] orphan_guard`); see the `handoff-trace`
skill.

## Later on

| where | what happens to a task with no requirement link |
|---|---|
| `handoff_trace_lint` | reported by the `task_orphan` rule (warning), with `task_id`, `title` and `suggested_action` |
| `handoff_update_task` / `handoff_bulk_update_tasks`, moving it to `review` or `done` | `[trace] orphan_guard`: `warn` (default) adds a `Warning [ORPHAN_TASK]` line, `block` rejects the call unless `force: true`, `off` does nothing |
| `handoff_report`, the `daily` / `weekly` / `monthly` / `quarterly` / `yearly` reports | listed in the `Orphan Tasks` section at the end of the report (count, then id, title and status per task); the section is absent when there is no such task |
| `handoff_get_task(task_id, include_orphan: true)` | the response carries `orphan: true` (`false` for every other task; the key is absent unless asked for) |
| the task-loop plugin's session-closer | the task is closed as `review` instead of `done`, with a `Requirement link missing: ...` note (see below) |

Never treated as missing a link: a task labelled `no-trace`, a `skipped` task,
and a parent task whose leaf tasks are all linked, `no-trace` or `skipped`.

If a line says a check failed (`Warning: orphan_guard check failed: ...`,
`Warning: requirement-link guidance check failed: ...`, or `Orphan tasks could
not be listed: ...` in a report), a document could not be read and the tool
could not tell whether the project uses traceability. Repair it with
`handoff_doc_repair_frontmatter`.

## In the dev cycle and the session loop

The requirement link is part of finishing a task, not an afterthought. In the dev cycle this
is Step 3b, right after the implementation and its tests:

- **Say which requirements the work addressed.** A developer report has a
  `### Requirements addressed` section in every case: one line per fact
  (`- <stable_id>: Implemented (<file>)`, `Tested`, `Result pass|fail`, `New-item`), or the
  single line `- None — <reason>` when no requirement id applies. The usual reasons are
  `new requirement needed` (the work implements something no requirement describes yet) and
  `no-trace` (the work is deliberately outside requirement traceability). The section is
  never left out or empty, and an id is never invented to fill it.
- **Link before closing.** Outside the session loop, link the task yourself before you mark
  it done: `handoff_update_task(task: {id: "<id>", requirement_ids: [...]})`. A `None` line
  is settled first: add the missing requirement and link it, or label the task `no-trace`.
- **The session-closer enforces it.** In `/session-loop` the session-closer applies the
  links the reports name and then asks the server about each task
  (`handoff_get_task(task_id, include_orphan: true)`). A task that is still an orphan is set
  to `review` instead of `done`, gets a `Requirement link missing: ...` entry in its notes,
  and is named in the session result (`requirement_link_missing:` in `warnings`). It is not
  a failed close-out; link the task or label it `no-trace`, then mark it done.
- A task labelled `no-trace`, a task with a requirement link, and a parent task whose
  subtasks are all accounted for are closed as usual. So is every task of a project with no
  layer document: nothing here applies until the project has one.

## Fixing it

1. `handoff_trace_propose(task_id: "<id>")` — candidates among the existing
   requirements, and a draft for a new one.
2. Link: `handoff_update_task(task: {id: "<id>", requirement_ids: ["REQ-012"]})`.
   `requirement_ids` replaces the whole set, so pass the ids the task already
   has together with the new ones.
3. Or exclude it on purpose: add `no-trace` to the task's `labels` (pass the
   labels it already has too).
