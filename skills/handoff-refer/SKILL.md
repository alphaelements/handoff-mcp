---
name: handoff-refer
description: "Send a cross-project referral to another project's .handoff/. Triggers on 'send referral', 'refer to <project>', 'cross-project', 'notify other project', or when work in one project reveals an issue/improvement needed in another."
---

# Handoff Refer Skill

Send a structured referral (improvement request, bug report, work request)
from the current project to another project that uses handoff-mcp.

## When to Use

- You discover a bug in a dependency project during work
- You identify an improvement needed in another project
- You want to request work from another team/project
- Cross-project coordination is needed

## Procedure

1. Identify the target project (by name or path).
2. Determine the referral type: `improvement`, `bug`, `request`, or `info`.
3. Call `handoff_refer` with:
   - `summary`: one-line description
   - `referral_type`: category
   - `priority`: `low`, `medium`, or `high`
   - `target_project` (name) or `target_project_dir` (path)
   - Optional: `details`, `tasks`, `context`
4. Report confirmation to the user.

## Target Resolution

- **By name**: `target_project: "pochi-dio"` — resolved via `scan_dirs` in config
- **By path**: `target_project_dir: "/home/user/pro/pochi-dio"` — direct path

Use name when the project is in a scan_dirs directory.
Use path when targeting a project outside scan_dirs.

## Managing Received Referrals

When `handoff_load_context` shows incoming referrals:

1. Review each referral's summary and priority.
2. Acknowledge with `handoff_update_referral` (status: `acknowledged`).
3. Create tasks based on the referral if appropriate.
4. Resolve with `handoff_update_referral` (status: `resolved`) when done.

## Example

```json
{
  "target_project": "handoff-mcp",
  "summary": "Import skill needs better task structuring guidance",
  "referral_type": "improvement",
  "priority": "medium",
  "details": "When importing 20 tasks from pochi-dio, done_criteria and links were mostly empty.",
  "tasks": [
    {
      "title": "Add before/after examples to import skill",
      "priority": "medium",
      "done_criteria": [
        {"item": "Skill includes concrete field mapping examples"}
      ]
    }
  ]
}
```
