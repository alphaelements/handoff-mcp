---
name: handoff-report-schedule
description: "Scheduled (recurring) report generation — set up weekly / monthly / daily / quarterly / yearly handoff_report runs with /schedule (CronCreate), cron expressions, recommended report parameters, re-rendering past reports as PDF, and the cloud-agent-only constraint. Triggers on '定期レポート', 'レポート自動生成', '週次レポートを自動で', '月次レポートを自動で', 'レポートをスケジュール', 'cron', '/schedule', 'CronCreate', 'scheduled report', 'recurring report', 'automate weekly report', 'automate monthly report', or when the user wants reports produced without asking each time."
---

# Scheduled Report Generation

`handoff_report` produces a report on demand. It has **no built-in scheduler**.
To get reports on a calendar (every Friday, the 1st of each month, ...), schedule an
*agent* that calls `handoff_report` for you with `/schedule` (the `CronCreate`
tool). This skill is the recipe.

## Constraint — cloud agents only

`/schedule` / `CronCreate` creates a recurring run of a **cloud agent**: the
agent starts in a fresh remote environment at each tick, not in your terminal.

- It does **not** run from a plain local CLI session. A local `claude` session that is
  closed, asleep or offline fires nothing, and no local process is started for you.
  If you need a purely local timer, use the OS scheduler (cron, launchd, Task
  Scheduler) to start your own agent run instead; this skill's prompt and parameters
  still apply.
- The cloud agent sees only what the remote environment gives it: a checkout of the
  repository and the MCP servers / connectors configured for that schedule. Your
  laptop's files, `~/.config`, and locally installed tools are **not** there.
- Therefore the `handoff` MCP server and the project's `.handoff/` data must be
  reachable from that environment (the repository contains `.handoff/`, or the
  data repository is what the schedule is attached to). If `.handoff/` is kept
  out of the project repository, attach the schedule to the repository that holds it.
- Files the run writes live only in that environment. **Commit and push**
  `.handoff/reports/` (and anything else you want to keep) at the end of the run, or the
  report is lost when the environment is discarded.
- `format: "pdf"` needs the Typst CLI (and `mmdc` for Mermaid charts). Do not assume
  the cloud environment has them. Default to Markdown there and render PDFs
  afterwards where Typst is installed (see "PDF" below).

Check the current `/schedule` documentation for the exact creation UI and the
timezone the cron expression is evaluated in before relying on a time of day.

## Which report, which cron

Use an **explicit `scope.period`** for periodic runs. The default period is the
*current* ISO week / month / quarter / year, which on the first morning of a new
period is nearly empty — a Monday-morning run with no period reports the week that just
started. Either run at the end of the period, or have the agent compute the
previous period and pass it.

| Goal | `report_type` | Cron (minute hour day month weekday) | `scope.period` the agent passes |
|------|---------------|--------------------------------------|---------------------------------|
| End-of-day log, weekdays | `daily` | `0 18 * * 1-5` | today, `2026-10-08` |
| Weekly, Friday close | `weekly` | `0 17 * * 5` | the current ISO week, `2026-W41` |
| Weekly, Monday look-back | `weekly` | `0 9 * * 1` | the **previous** ISO week |
| Monthly, 1st of the month | `monthly` | `0 9 1 * *` | the **previous** month, `2026-09` |
| Quarterly | `quarterly` | `0 9 1 1,4,7,10 *` | the **previous** quarter, `2026-Q3` |
| Yearly | `yearly` | `0 9 2 1 *` | the **previous** year, `2025` |

Standard cron cannot say "last day of the month"; schedule the 1st and report on the
month that just ended. The daily report's default day is *today in UTC*, so pass the
date explicitly when your cron timezone is not UTC.

## The scheduled prompt

Give the schedule a self-contained prompt; the run has no memory of earlier ticks.
Example for a weekly Monday run:

```
Generate last week's handoff report and publish it for review.

1. Work out the previous ISO week (Monday..Sunday) from today's date, e.g. 2026-W40.
2. Call handoff_report(action: "generate", report_type: "weekly",
   scope: {period: "<previous ISO week>"}, include_charts: true,
   data: {ai_summary: "<3-5 sentence summary of the week>",
          ai_risks: "<open risks, or omit>"}).
3. Call handoff_report(action: "submit", report_id: "<id from step 2>") so a human can
   approve it. Do NOT approve or publish it yourself.
4. Commit and push .handoff/reports/ with the message "report: weekly <period>".
5. If any step fails, stop and print the error; do not retry with different arguments.
```

Swap `report_type` / `period` for the other rows of the table.

## Recommended parameters

| Parameter | Recommendation | Why |
|-----------|----------------|-----|
| `report_type` | `weekly`, `monthly`, `daily`, `quarterly`, `yearly` | these collect their own data from tasks, time log, events and metrics snapshots — no `data` needed |
| `scope.period` | always explicit | defaults point at the period still in progress |
| `include_charts` | `true` (default) | burndown / velocity charts; they are Mermaid blocks, harmless in Markdown |
| `data.ai_summary`, `data.ai_analysis`, `data.ai_risks`, `data.ai_recommendations` | let the agent write them | rendered as AI sections right after the header; omit a key to hide its section |
| `sections` | only to trim or reorder | omit for the template's full report |
| `report_template` | a custom `.handoff/templates/<name>.md.hbs` for a house style | check names with `action: "templates"` |
| `format` | `"markdown"` in the cloud | PDF needs Typst, usually missing there |
| follow-up action | `submit` only | keeps the approve / publish decision with a human |

`handoff_report` stores each run as a new report (the version increases per type and
scope), so a repeated tick does not overwrite earlier reports.

## PDF: re-render any past report

A stored report can be turned into a PDF at any time, from any machine that has
the report files and Typst — the Markdown body is rendered as it was stored,
nothing is regenerated and the report's status and version do not change:

```
handoff_report(action: "get", report_id: "<id from action: list>", format: "pdf",
               cover_version: "v1.2", cover_status: "Approved",
               output_dir: "deliverables")
```

The response gains `pdf: {pdf_path, typ_path, warnings}`; the PDF defaults to
`.handoff/exports/<report_id>.pdf`. Find ids with
`handoff_report(action: "list", report_type: "weekly")` (newest first).
This is the intended split for scheduled reports: **generate Markdown in the cloud,
render PDFs locally** when someone needs the deliverable.

## First-run checklist

1. Run the scheduled prompt once by hand in the same environment and check that
   `handoff_report(action: "list")` shows the new report and that the push happened.
2. Confirm the cron timezone and that the explicit `scope.period` is the one you meant.
3. Make sure the run cannot approve or publish; reports stay `draft` / `submitted`
   until a human acts (`approve`, `reject`, `publish` are separate actions).
4. After the first scheduled tick, check the report exists on the remote.

## Related

- `handoff-trace` — report types that need traceability data (verification,
  inspection, completion) and `handoff_report` with milestones.
- `handoff` — task and milestone setup that the periodic reports read.
