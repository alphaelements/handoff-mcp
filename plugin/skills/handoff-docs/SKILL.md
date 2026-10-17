---
name: handoff-docs
description: "Document management — save, read, search, import, and traverse structured project documents (specs, designs, plans, ADRs, guides, notes). Triggers on 'ドキュメント保存', '仕様書を管理', '設計書をインポート', '実装計画', 'save this doc', 'import specs', 'document management', 'タスク開始', 'spec registration', '仕様登録', 'verification check', 'scaffold_suggestions', 'schedule_stale', 'orphaned_tasks', or when the user asks to persist/organize/search multi-section markdown that is too structured for a single memory entry, or when the AI writes a spec/design/plan document during development."
---

# Handoff Docs Skill

Structured project documents under `.handoff/docs/`: one Markdown file per
document, indexed by section, searchable, linked to tasks and to each other.

- Per-tool parameters: [references/doc-tools.md](references/doc-tools.md)
- Section verification matrix and `doc_req_*` requirements:
  [references/verification-matrix.md](references/verification-matrix.md)
- V-model layer documents (requirements → specs → tests with coverage,
  suspects, approval): the `handoff-trace` skill (overview, setup) and the
  `handoff-requirements` skill (how to write items)

## When to use

- The user asks to save a spec, design, plan, ADR, guide or note so it
  survives across sessions — or you write one during development
- Importing an existing directory of Markdown (`specs/`, `docs/`) at once
- Reading one section of a large document without loading all of it
- Following how documents relate (parent/child, "implements", "supersedes")
- Linking a document to tasks so it surfaces while those tasks are worked on

A short standalone lesson/rule/gotcha (under a page, no sections) belongs in
project memory instead — see the `handoff-memory` skill and "Memory vs
Documents" below.

## Choose a tracking style first

| you need | use |
|---|---|
| keep a document, read and search it | plain document (`doc_save`) |
| tick off sections of a spec as implemented | **section matrix** (`doc_verify`, see verification-matrix.md) |
| a list of individually tracked requirements from a requirement table | section matrix + `doc_req_import` (verification-matrix.md) |
| requirements linked to specs and tests, coverage, change impact (suspects), approval | **V-model layer document** — `doc_save(layer=...)`, see `handoff-trace` |

A layer document's items come from its body headings; the section matrix's
`add_item` / `set_priority` / `set_refs(test_refs)` and `doc_req_import` are
refused on it. Pick one style per document.

If the project uses the V-model (`[trace] profile` is set — check
`handoff_get_config`), save specs and designs **with a `layer`**, or enable
`[trace] auto_layer` so `doc_save` infers one: `spec` → `requirement`,
`design` → `detailed_spec`, `plan` → `detailed_spec` (override with
`[trace] plan_default_layer`); `adr` / `guide` / `note` never get one. An
explicit `layer` always wins (`""` clears it and suppresses inference for that
call). For layers the table does not cover (`basic_spec`, `acceptance`, …),
always pass `layer` explicitly.

## Development flow integration

Use documents as part of normal work, not only on request:

| moment | do |
|---|---|
| you write or update a spec/design/plan | `handoff_doc_save(slug=..., title=..., body=..., doc_type=..., task_ids=[...])` — plus `layer=` in a V-model project; otherwise `doc_verify(action="generate")` if you want a section checklist |
| starting a task | `handoff_doc_query(task_id="...")` surfaces linked documents; V-model: `handoff_trace_slice(task_id=...)` |
| implementation done | section matrix: `doc_verify(check / set_refs)`; V-model: record results (`handoff-trace` → verification) |
| review | `handoff_task_checklist(task_id=..., action="view")` — done_criteria + linked matrices in one view |
| test results available | `handoff_trace_ingest(format: "junit_xml", output_file: ...)` for layer documents |
| code ↔ requirement links after implementation | `handoff_doc_req_scan(scope_paths=[...])`, review, `doc_verify(set_refs)` |

## Document creation rules

1. **One document = one complete Markdown document** starting with `# Title`.
   Group same-kind content into one document — ADR-001…ADR-005 are `##`
   sections of one `# Architecture Decision Records`, not five documents.
2. **Add sections with `append_body`**, not by resending the whole body:
   `doc_save(doc_id="doc-...", append_body="## ADR-006: ...")`.
3. **Don't pre-split.** Sections are indexed automatically at each `##`
   (configurable with `split_level`); fetch one with
   `doc_get(doc_id, format="section", seq=N)`.
4. **Create vs update**: create = omit `doc_id`, give a unique `slug`
   (`[a-z0-9-]`, 1–60, chosen explicitly). Update = pass the existing `doc_id`
   (the id, not the slug — `doc_save` does not resolve slugs). The slug cannot
   be renamed.

## The doc tools

| tool | purpose |
|---|---|
| `handoff_doc_save` | create/update/append; metadata-only update when body is omitted |
| `handoff_doc_get` | `full` body, `meta` only, or one `section` by `seq` |
| `handoff_doc_list` | filter by `doc_type` / `tags` / `task_id`, BM25 `query`; reports unreadable files |
| `handoff_doc_delete` | delete a document, unlink tasks, orphan (not delete) children |
| `handoff_doc_update_section` | replace/delete one section by `seq`, optional optimistic lock |
| `handoff_doc_reassemble` | read the original body, detect out-of-band edits, optionally write it out |
| `handoff_doc_tree` / `handoff_doc_trace` | family-tree traversal / lineage chain |
| `handoff_doc_graph` | all documents as a graph (parent, related, implicit shared-task/scope edges) |
| `handoff_doc_query` | hook-style context injection, staged full/outline |
| `handoff_doc_analyze` → `handoff_doc_import` | bulk import: read-only scan, review, atomic write |
| `handoff_doc_verify` / `handoff_doc_verify_status` | section verification matrix |
| `handoff_doc_req_import` / `_list` / `_status` / `_scan` / `_impact` | individually tracked requirements |
| `handoff_doc_repair_task_ids` / `handoff_doc_repair_frontmatter` | repair drifted task links / malformed frontmatter |

Details for each: [references/doc-tools.md](references/doc-tools.md) and
[references/verification-matrix.md](references/verification-matrix.md).

## `doc_type` values

`spec` (requirements, behaviour contracts) · `design` (architecture) · `plan`
(implementation plans; see "Plan lifecycle notifications") · `adr` · `guide`
(how-to, operations) · `note` (anything else).

When omitted on create, it is inferred from slug, title and tags by keyword
(priority `spec` > `design` > `plan` > `adr` > `guide`, else `note`) — "P4
Implementation Plan" becomes `plan`. Updates keep the existing type.

## Staged injection

`handoff_doc_query` keeps large documents from flooding context:

| mode | when | injected |
|---|---|---|
| `full` | the section is small (within the inline token threshold, default 300) | metadata + whole section |
| `outline` | larger sections | metadata + headings only — fetch with `doc_get(format="section", seq=N)` |

Override per document with `auto_inject` on `doc_save`: `auto` (default),
`full`, `outline`, or `none` (only via explicit `doc_get`).

## Family tree

- **parent/child** (`parent_id`) — structural hierarchy (e.g. a directory
  document and its files).
- **related** (`related: [{id, rel}]`) — semantic links: `supersedes`,
  `references`, `implements`, `extends`, `conflicts`.

`handoff_doc_tree(include_related: true)` walks both; `handoff_doc_trace`
gives an ordered root-to-leaf chain.

## Task linking — two levels

| level | how | what it means |
|---|---|---|
| document | `doc_save(task_ids: [...])` | the document as a whole relates to these tasks (bidirectional `TaskLink{link_type: "doc"}`); removed automatically on delete |
| requirement | `handoff_update_task(task: {id, requirement_ids: [...], requirement_roles?: {...}})` | the task implements (or executes the verification of) specific items/SubItems — this is what coverage, `dev_stage` and the VS Code Requirements Explorer use |

- `requirement_ids` on an existing task **replaces the set**: new ids are
  linked, missing ones unlinked (both sides).
- Role `implements` (default for definition items) moves the item's
  `dev_stage` with the task's status; `executes` (inferred for verification
  items) never does. Linked tasks get a `trace` field in `handoff_get_task`
  and are subject to the done guard — details in `handoff-trace` →
  `references/verification.md`.
- Do not use `doc_save(task_ids=...)` to record that a task implements a
  requirement — that only creates the document-level link.
- Lookup: `handoff_doc_list(task_id: ...)` for documents of a task;
  `handoff_get_task` → `task_links` for a task's targets. There is no
  "tasks linking to this document" filter — scan `task_links` yourself.

## Plan lifecycle notifications

Saving a `plan` document (or changing a task linked to plan requirements)
returns suggestion fields — nothing is applied automatically:

| field | returned by | meaning | do |
|---|---|---|---|
| `scaffold_suggestions` | `doc_save` (plan documents) | requirement items no task links to yet, `[{stable_id, title}]` | link an existing task or create one with `requirement_ids` |
| `schedule_stale` | `doc_save` (task links changed), `update_task` (status, dependencies, dates changed) | dates should be recomputed | `handoff_auto_schedule(dry_run: true)`, then apply |
| `orphaned_tasks` | `doc_save` that removed requirement items | tasks that lost all requirement links, `[{task_id, title}]` | re-link, or close/skip if the requirement was dropped on purpose |
| `baseline_suggestions` | `trace_suspect(action: "clear")` | cleared tasks whose schedule baseline is missing or stale (`set_recommended` / `update_recommended` / `manual_baseline_detected`) | `handoff_update_task(task: {id, schedule: {baseline_start, baseline_due}})`; flag `manual_baseline_detected` to the user |

## Import workflow (3 steps)

Use this rather than file-by-file `doc_save` for an existing pile of
Markdown — relationships and duplicates can only be checked with every file
visible.

1. `handoff_doc_analyze(path, recursive, flatten)` — read-only report:
   `auto_resolved` guesses and `needs_review` items, each with a suggestion.
2. Review: accept, correct or reject each suggestion. Nothing is written yet.
3. `handoff_doc_import(analyzed, overrides, task_ids)` — atomic write of the
   whole set, including task links.

## Diagnostics

`doc_*` tools may return structured warnings `{severity, code, message,
fix_hint?, affected_doc_ids?}` — handle them per the `handoff` skill's
"Diagnostics & Warnings Handling". Codes `DIAG-R001`, `DIAG-R002`,
`DIAG-D001` are explained in
[references/verification-matrix.md](references/verification-matrix.md#diagnostics).
An empty `doc_req_list` with a warning is a setup issue, not "no
requirements".

## Writing from a template

Keep reusable outlines as ordinary documents (for example
`doc_type: "guide"`, `tags: ["template", "spec"]`, `auto_inject: "none"` so
they are fetched on demand rather than injected), and start new documents from
them with `handoff_doc_list(tags: ["template"])`. For V-model layer documents,
the `handoff-requirements` skill ships per-layer templates.

## Memory vs Documents

| criterion | memory | document |
|---|---|---|
| size | under a page, no sections | multi-section |
| lifecycle | durable lesson/rule | evolves with the project |
| granularity | one fact/convention | sections tracked independently |
| review tracking | none | verification matrix / V-model |
| task linkage | none | bidirectional |
| example | "always push over SSH" | "authentication spec with 5 sections" |
