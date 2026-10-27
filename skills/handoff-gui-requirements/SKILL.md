---
name: handoff-gui-requirements
description: "GUI 要件管理 — brainstorm から VRT 検証完了まで。Triggers: GUI要件, GUI requirement, mock属性, ゴールデン, VRT, ビジュアルリグレッション, gui-vrt, 画面設計, ワイヤーフレーム, GUI verification pipeline, mock/golden/viewport"
---

# Handoff GUI Requirements (end-to-end navigator)

How a GUI screen travels from an idea to a recorded visual-regression verdict:
discuss the screen, write requirements that carry `mock` / `golden` /
`viewport` / `capture_method` attributes, let `handoff_trace_tasks` generate a
`[VRT]` verification task, run the VRT, record the verdict in the trace graph,
and read it back in the milestone report. This skill only says **which step
uses which tool and which skill**; every step's details live in the skill it
links.

## Pipeline

1. **Discuss the screen.** `/brainstorm-loop ... project_type: gui-app`
   (also `gui-desktop` / `gui-mobile`) promotes the GUI topics (screens,
   wireframes, states, viewports). Skill: `handoff-task-loop:brainstorm-loop`.
2. **Write GUI requirements.** Run requirement-loop (or edit a layer document)
   and give each screen item the attribute lines `mock` (required for VRT),
   `golden`, `viewport` (`WxH`), `capture_method`. Skill: `handoff-requirements`
   §4 "GUI requirement attributes (VRT)"; documents: `handoff-docs`.
3. **Generate tasks.** `handoff_trace_tasks(items, default_milestone, mode:
   "apply")` creates the normal task plus a `[VRT] <id> <title>` task
   (labels `vrt`, `gui-verification`; 3 done_criteria: golden captured, VRT
   PASS, AI visual PASS). `default_milestone` puts both on a milestone.
   Skill: `handoff-trace` "GUI verification (VRT tasks)".
4. **Run the verification.** `gui-vrt` modes: `capture-golden` (mock -> golden,
   user approves), `verify` (real UI vs golden, pixelmatch), `ai-compare`
   (LLM verdict when pixel diff is too strict or no golden). Skill: `gui-vrt`;
   agent: `gui-visual-verifier`. Check off the `[VRT]` task's criteria as each
   is really done (`handoff_check_criterion`).
5. **Record the result.** `handoff_trace_record(results: [{item, result}])`
   for the verdict in the trace graph; `handoff_trace_test_run(action:
   "add_evidence", ...)` with a `vrt` block for baseline / actual / similarity
   evidence. Skill: `handoff-trace`, `references/tools.md`.
6. **Report.** `handoff_report(action: "generate", report_type: "milestone",
   scope: {milestone: "<id>"})` — includes the **GUI Verification (VRT)**
   section (`golden` / `vrt` / `ai_visual` per task) when the milestone has
   `vrt` tasks. Skill: `handoff` (Milestone workflow), `handoff-trace`.

## Example

Requirement in a layer document (attribute lines directly under the heading):

```markdown
### FR-GUI-001 Login screen

- mock: docs/mocks/login.html
- golden: .handoff/evidence/golden/login/v1/default.png
- viewport: 1280x720
- capture_method: playwright

The login screen shows ...
```

Command sequence:

```
handoff_trace_tasks(items: ["FR-GUI-001"], default_milestone: "v1.0",
                    mode: "apply", estimate_hours: 2)      # task + [VRT] task
gui-vrt skill, mode capture-golden (docs/mocks/login.html)  # golden, user approves
gui-vrt skill, mode verify (feature: login)                # pixelmatch vs golden
handoff_trace_record(results: [{item: "FR-GUI-001", result: "pass"}])
handoff_trace_test_run(action: "add_evidence", test_run_id: "...",
  item_id: "AT-GUI-1",
  evidence: {path: ".handoff/evidence/diff/login/20261027-101500/default.png",
             type: "vrt-diff",
             vrt: {baseline_path: ".handoff/evidence/golden/login/v1/default.png",
                   actual_path: ".handoff/evidence/actual/login/20261027-101500/default.png",
                   threshold: 99, similarity_pct: 99.5,
                   viewport: "1280x720", tool: "pixelmatch"}})
handoff_report(action: "generate", report_type: "milestone", scope: {milestone: "v1.0"})
```

## Related skills

- `handoff-requirements` — attribute syntax, IDs, acceptance criteria
- `handoff-docs` — layer documents and requirement items
- `handoff-trace` — `trace_tasks`, `trace_record`, `trace_test_run`, reports
- `gui-vrt` — capture-golden / verify / compare / ai-compare, setup guide
- `handoff-task-loop:brainstorm-loop` — GUI-aware brainstorming
