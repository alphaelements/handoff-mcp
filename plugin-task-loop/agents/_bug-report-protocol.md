# Bug/Improvement Report Protocol (shared)

Shared rules for session-developer / session-tester / session manager to ensure bugs and
improvements found during work are **never silently swallowed** but recorded as new
handoff tasks.

This file is referenced by each agent definition.

## When to create a report task

**Whenever you find something outside the current task's scope:**

- **Bug**: Existing/new code defects, regressions, spec deviations, unhandled edge cases.
- **Improvement**: Refactoring opportunities, performance, UX, test gaps, convention violations, tech debt.
- **Spec ambiguity/contradiction**: Discrepancies between spec documents or between spec and code.

Decision criteria:

- **Trivial and in-scope** -> fix it on the spot (don't create a separate task).
- **Out of scope or deserves independent tracking** -> create a report task.
- When in doubt, **create it** (don't swallow it). But check for duplicates first via
  `handoff_list_tasks`.

## How sub-agents (developer / tester) report

Sub-agents don't call handoff directly. Instead, **include structured findings in the
return value**. The manager creates the actual handoff tasks. Include this section in
your report:

```
### Discovered issues (report task candidates)
- **[bug|improvement|spec] title**
  - Description: <what's wrong. Reproduction steps if applicable>
  - Location: <file:line range> (multiple OK)
  - Spec link: <wiki/xx.md#section or docs/plans/... if applicable>
  - Proposal: <current -> proposed -> benefit>
  - Severity: high|medium|low
  - Relationship to current task: <why this is a separate task, not an in-scope fix>
```

If nothing found, explicitly write: `### Discovered issues: None` (don't stay silent).

## How the session manager creates report tasks

When receiving "Discovered issues" from sub-agents, **before closing the task**, create
new tasks via `handoff_update_task` (omit id for auto-assignment):

- `title`: `[bug]` / `[improvement]` / `[spec]` prefix + concise title
- `status`: `todo` (backlog only — don't start working on it)
- `priority`: Match the sub-agent's severity (high/medium/low)
- `labels`: `["found-during-loop", "<type>"]` (for later filtering)
- `links`: File paths (`file:line`) + spec document paths/URLs
- `notes`: Description, reproduction steps, `current -> proposed -> benefit` proposal,
  relationship to the originating task. **Include originating task ID and session ID**
  for traceability.
- `done_criteria`: At least one criterion defining "what makes this resolved".

After creation:

- Record "Created report task <new_id>" in `handoff_save_context` notes.
- Inform the user: "Filed N issues as tXX" in the session summary.

## Rules

- **Never swallow findings.** "Noticed but didn't report" is a quality incident waiting to happen.
- **No duplicates.** Check `handoff_list_tasks` before creating.
- Report tasks go to **backlog (todo) only**. Don't start them in the current loop
  (adding them to the goal scope causes scope creep).
