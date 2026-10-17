---
name: handoff-trace
description: "V-model traceability — overview, first-time setup, profiles, coverage, suspects, verification runs and campaigns, approval, baselines, reports, and the trace_* tool family. Triggers on 'トレーサビリティ', 'V字モデル', 'V字を始めたい', 'カバレッジ', '要件リンク', 'suspect', '検証キャンペーン', '承認', '報告書', 'trace_report', 'trace_update', 'traceability', 'requirement coverage', 'V-model', 'verification campaign', or when a task mentions layer documents, acceptance criteria blocks, or verification matrices."
---

# Handoff Trace Skill (V-model traceability)

The entry point for the V-model system built on top of `handoff_doc_*`:
what it is, how to switch it on, how a project moves through it, and which
tool to call. Details live in reference files — read the one you need:

| need | read |
|---|---|
| **Writing** requirements / specs / test items, IDs, attributes, requirement trees | the `handoff-requirements` skill |
| Every trace tool's parameters and outputs, CLI equivalents | [references/tools.md](references/tools.md) |
| Every `[trace]` config key, layers, profiles, `auto_layer` | [references/config.md](references/config.md) |
| Every lint rule and how to fix it, policy rules, `quality_prompt` | [references/lint-rules.md](references/lint-rules.md) |
| Recording results, test ingestion, campaigns, approval, layer status, done guard, bug flow, reports | [references/verification.md](references/verification.md) |
| Hashes, baselines, suspects, impact analysis, change proposals, sync rules, data locations | [references/change-management.md](references/change-management.md) |
| Moving `doc_req_import` requirements into a layer document; upgrading older projects | [references/migration.md](references/migration.md) |
| One requirement carried end to end, with every call | [references/walkthrough.md](references/walkthrough.md) |

## 1. Mental model

```
 definition (left)                              verification (right)
 requirement   REQ/FR/NFR  ── verified by ──▶  acceptance   AT/AC
   │ refined by                                    ▲
 basic_spec    SPEC/BS     ── verified by ──▶  system_test  ST
   │ refined by                                    ▲
 detailed_spec DS          ── verified by ──▶  unit_test    UT
        ▲ implements                                ▲ executes
      tasks ────────────────────────────────────── tasks
```

- **Layer document** — a document saved with `doc_save(layer=...)`. Its
  Markdown body is the source of truth; each `### <ID> title` heading is one
  **item**, and the bullet list right under it holds the item's attributes.
- **Links** — `refines` (detail an upper left item), `verifies` (a right item
  checks a left item, optionally one acceptance criterion `REQ-003#AC2`), and
  `parent` (decompose within one layer: the requirement tree).
- **Acceptance criteria** — a `受入基準:` / `Acceptance criteria:` block under
  an item; under the `minimal`/`bugfix` profiles each criterion becomes its own
  verification item automatically.
- **Tasks** — `handoff_update_task(requirement_ids=[...])` links a task to items
  with role `implements` (left side) or `executes` (right side).
- **Three independent axes per item**: `dev_stage` (implementation, driven by
  implementing tasks), `state` (verification, from recorded results:
  passing / failing / blocked / not_run / uncovered), `approval`
  (draft → review → approved; editing the definition resets it to draft).
- **Coverage** — horizontal (does it have a verifier?) and vertical (is it
  refined downward?), each `covered` / `partial` / `waived` / `uncovered` /
  `na`. Under implicit-acceptance profiles "covered" is not "verified" — read
  `state` for that.
- **Suspects** — every link remembers the hash of what it pointed at. When the
  upstream definition changes, the link (and linked tasks, and passing results)
  become **suspect** until someone reviews and clears them. Spread is one hop.
- **Project-level** — layer statuses (per-layer approval lifecycle), project
  baselines (coverage snapshots), verification campaigns (manual checklists
  with evidence), change proposals (deltas), and generated reports.

## 2. First-time setup

`handoff_init` writes **no `[trace]` section**: no profile, layers
auto-detected from whatever layer documents exist, `done_guard = "warn"`.
`handoff_load_context`'s `trace_health` shows `has_data: false` until the
first layer document exists.

**Step 1 — pick a profile.**

| your project | profile | you maintain |
|---|---|---|
| small tool, AI-driven work, want the lightest V | `minimal` | requirement documents with acceptance criteria (verification items generated) |
| product with specs and system tests | `standard` | requirement + basic spec + acceptance + system test documents |
| safety/regulated, full decomposition | `full` | all six layers |
| a bug-fix stream | `bugfix` (often per document) | reproduction (requirement) + regression test (acceptance) |

**Step 2 — write the config** (dot-notation keys through `updates`):

```
handoff_update_config(updates: {
  "trace.profile": "standard",
  "trace.id_prefixes.requirement": ["SYS", "UI"],   # only if you use scope prefixes
  "trace.done_guard": "warn"                        # "block" to enforce
})
```

Optional: `"trace.auto_layer": true` makes `doc_save` infer a layer from
`doc_type` for documents saved without one (`spec` → requirement, `plan` →
detailed_spec, …; table in [references/config.md](references/config.md)).
Custom layers, custom profiles and lint policy are also in config.md.

**Step 3 — write the first layer document** (see `handoff-requirements`):

```
handoff_doc_save(slug: "auth-requirements", title: "Auth Requirements",
                 layer: "requirement", body: "# Auth Requirements\n\n### FR-001 ...")
```

**Step 4 — check**: `handoff_trace_report()` — `trace_layers.source` should be
`profile` (or `config`), and the item should appear. `handoff_trace_lint()`
lists what is still missing.

**Existing project?** Documents you imported with `doc_req_import` need
[references/migration.md](references/migration.md); a project that used layer
documents with an older release should run the baseline step there too.

## 3. How a project moves through the V

| stage | what happens | main calls |
|---|---|---|
| 1. Raise requirements | write items with acceptance criteria; check duplicates first | `trace_propose`, `doc_save(layer="requirement")` |
| 2. Decompose | requirement tree: scope → feature → branch → leaf | `trace_propose(parent=...)`, `trace_tree` |
| 3. Specify | basic/detailed spec items that `refines:` upward | `doc_save` / `trace_update(upsert_item)` |
| 4. Design verification | test items that `verifies:`, or generated from criteria | `trace_scaffold`, hand-written AT/ST/UT |
| 5. Plan work | tasks linked to items | `trace_tasks`, `update_task(requirement_ids)` |
| 6. Implement | status transitions drive `dev_stage` | `update_task`, `trace_slice(task_id)` |
| 7. Verify | record automated and manual results | `trace_ingest`, `trace_update(record)`, `trace_test_run` |
| 8. Handle change | review what an edit affects, clear or fix suspects | `trace_impact`, `trace_suspect`, `trace_delta` |
| 9. Approve | items → layers → campaign | `trace_update(set approval / set_layer_status)`, `trace_test_run(set_status)` |
| 10. Report & release | snapshot, export, generate reports | `trace_baseline`, `trace_matrix`, `handoff_report` |

At any point: `trace_next` ranks what to do next, `trace_lint` finds
structural and quality problems, `trace_report` gives the whole picture.

### AI working loop (per task)

1. `handoff_trace_slice(task_id: "t12")` — the items this task implements or
   executes, their upstream and verifiers (small context).
2. If the task has no linked item yet: `trace_propose(task_id: "t12")` → write
   the item (or reuse a candidate) → `update_task(task: {id, requirement_ids})`.
3. Implement; run the tests named in the items' `test:` attributes.
4. Record results — in bulk with the rest of your edits
   (`trace_update(ops: [{op: "record", item, result: "pass"}, ...])`) or from
   the test runner output (`trace_ingest(format: "junit_xml", output_file: ...)`).
5. Before moving the task to review/done: `trace_next(task_id: "t12")` — fix
   failing verifiers, review suspects, add missing verification. The done guard
   (see verification.md) reports the same blockers on the status change.
6. Never add `derived` / `waive-verify` / `waive-refine` exemptions without the
   user's agreement — they are surfaced as `waiver_added` warnings.

### Human operating flow (with the Handoff VS Code extension)

| stage | screen | MCP / CLI |
|---|---|---|
| raise / expand requirements | Documents | `doc_save`, `trace_propose` |
| design verification | Documents / Trace | `trace_scaffold` |
| plan | Plan board | `trace_tasks`, `update_task(requirement_ids)` |
| run checks | Inspector (session) | `trace_ingest`, `trace_record`, `trace_test_run` |
| review suspects | Trace | `trace_suspect(action: "clear")` |
| approve | Inspector (item) | `trace_update` `set approval` / `set_layer_status` |
| report | — | `handoff-mcp report generate ...` |

Write paths from the extension go through MCP/CLI (suspect clearing through
`trace_suspect`, matrix export through `handoff-mcp trace matrix`). A task
moved to done by editing its file directly skips the done guard — check
`_trace_report.json`'s `tasks[].blockers` first.

## 4. Tool map

All `handoff_trace_*` tools plus `handoff_report`. Full signatures, outputs and
CLI names: [references/tools.md](references/tools.md).

| need | tool | writes? |
|---|---|---|
| whole-graph coverage, gaps, items; refresh `_trace_report.json` | `trace_report` | resyncs edited docs, writes the report cache |
| neighbourhood of one task/item | `trace_slice` | resyncs edited docs |
| nested requirement tree with roll-ups | `trace_tree` | no |
| CSV/Markdown matrix export | `trace_matrix` | only `output_file` |
| ranked next actions | `trace_next` | no |
| what an edit would affect | `trace_impact` | no |
| structural / tailoring / tree / quality lint; LLM quality review prompt | `trace_lint` | no |
| list / clear suspects, baseline unbaselined links | `trace_suspect` | `clear`, `baseline` |
| duplicate check + template for a new item / tree child | `trace_propose` | no |
| generate verification items from acceptance criteria | `trace_scaffold` | `mode: "apply"` |
| generate tasks for items missing one | `trace_tasks` | `mode: "apply"` |
| bulk edit: items, links, runtime fields, results, suspect clears, layer status | `trace_update` | yes (or a pending delta with `propose: true`) |
| record results (single call) | `trace_record` | yes |
| ingest cargo JSON / JUnit XML test output | `trace_ingest` | yes |
| an item's result timeline | `trace_history` | no |
| verification campaign: scope, checklist, evidence, approval | `trace_test_run` | yes |
| project snapshots, trend, diff, regressions | `trace_baseline` | `create` |
| change proposals: create, apply (partially), reject | `trace_delta` | yes |
| verification / inspection / defect / completion / periodic reports | `handoff_report` | yes |

Related tools elsewhere: `handoff_update_task` (`requirement_ids`,
`requirement_roles`, `force`), `handoff_get_task` (`trace` field),
`handoff_list_tasks` (`layer` / `role` filters), `handoff_doc_save` (`layer`,
`trace_profile`), `handoff_get_config` / `handoff_update_config`.

## 5. Profiles in one paragraph

Used layers resolve as: explicit `[trace] layers` > the project `profile`'s
layers > auto-detection (a warning is emitted when both `layers` and `profile`
are set). A document may override with `doc_save(trace_profile: "bugfix")`
(empty string clears it); the override applies to **every item reachable from
that document's items**, and an item reachable from several roots gets the
union of their layers (the stricter side wins). An item whose layer is not in
its effective set drops out of coverage (`layer_outside_profile`, info).
Details, custom profiles, `default_needs`: [references/config.md](references/config.md).

## 6. Diagnostics and empty results

Trace tools return `warnings` that may be structured
(`{severity, code, message, fix_hint?}`) — handle them as described in the
`handoff` skill's "Diagnostics & Warnings Handling".

| code | meaning | fix |
|---|---|---|
| `DIAG-T001` | no `[trace]` layers/profile configured and nothing auto-detected | §2 step 2 — e.g. `handoff_update_config(updates: {"trace.profile": "minimal"})`. Layer ids are `requirement`, `basic_spec`, `detailed_spec`, `acceptance`, `system_test`, `unit_test` |
| `DIAG-T002` | no layer is in use: no document has a `layer` | `handoff_doc_save(doc_id=..., layer="requirement")` on an existing document, or write a new one (ignore a hint suggesting `"requirements"` — that is not a layer id) |
| `DIAG-T003` | some `_doc.<slug>.md` failed to parse, so its items are missing | `handoff_doc_repair_frontmatter(dry_run=true)`, then `dry_run=false` |
| `DIAG-R001` (load_context / doc tools) | documents with no `layer` and no verification matrix | set a layer, or see migration.md if they hold `doc_req_import` items |

An **empty** `trace_report` / `trace_slice` / `trace_next` means "nothing to
do" only when there is no warning above `info`. A `DIAG-T00x` means a setup
problem — fix it rather than concluding the project has no requirements.
`trace_lint`'s `findings` are its result; its own `warnings` are about the
call. An unknown rule id in `rules` is an error, not a warning.

## See also

- `handoff-requirements` — authoring items (templates, attributes, trees).
- `handoff-docs` — the generic document tools layer documents are built on.
- `handoff` — sessions, tasks, time tracking.
- The `handoff-task-loop` plugin's session-loop consumes `trace_slice` /
  `trace_propose` / `trace_next` / `trace_update` across a session.
