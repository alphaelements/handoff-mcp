# Verification matrix and req_* requirements

The **section-matrix path**: a per-section checklist on an ordinary (non-layer)
document, optionally with individual requirements (SubItems) imported from a
requirement table. Use it for lightweight spec tracking. For linked
requirements → specs → tests with coverage, suspects and approval, use a
V-model layer document instead (`handoff-trace` skill); the two are not
combined on one document.

## `handoff_doc_verify`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document whose matrix to operate on |
| `action` | yes | `generate`, `check`, `check_all`, `skip`, `sync`, `set_refs`, `set_dev_stage`, `set_priority`, `add_item`, `backfill_stable_ids`, `suggest_refs` |
| `fragment_seq` | for section-level actions | Section number (an array is allowed for `check`). May be omitted when `sub_item_id` addresses a SubItem. For `add_item`, omit it to add a freeform item not tied to a section |
| `sub_item_id` | no | Address a SubItem by its `stable_id` (searched across the whole matrix, freeform items included). Preferred over `sub_item_index` |
| `sub_item_index` | no | Address a SubItem by its 0-based index within `fragment_seq` |
| `description` | `add_item` with `fragment_seq` | The new SubItem's text |
| `label` | `add_item` without `fragment_seq` | The new freeform item's label |
| `category` | no | `add_item`: `requirement` (default for SubItems), `visual`, `regression`, `manual`, … |
| `skip_seqs` | `generate` | Sections to mark skipped at creation (e.g. `[0]` for the preamble) |
| `reviewer` | no | `ai` or `user` |
| `notes` | no | Notes on the check |
| `impl_refs` / `test_refs` | `set_refs` | `[{path, lines?, label?}]` |
| `dev_stage` | `set_dev_stage` | `not_started` / `in_progress` / `implemented` / `tested` / `verified` |
| `priority` | `set_priority` | `P0`–`P3` |

| action | effect |
|---|---|
| `generate` | create the matrix from the sections (fails if one exists — use `sync`) |
| `check` | mark sections, or one SubItem, `verified` (records `verified_at`, the content hash at verification) |
| `check_all` | mark every section and SubItem verified |
| `skip` | mark a section or SubItem not applicable |
| `sync` | rebuild after sections were added/removed; keeps statuses and freeform items. On a layer document it runs the layer body sync instead |
| `set_refs` | attach implementation/test locations |
| `set_dev_stage` / `set_priority` | set a SubItem's field (`sub_item_id` or `sub_item_index` required) |
| `add_item` | with `fragment_seq`: add a SubItem to that section; without: add a freeform item (GUI check, regression sweep) |
| `backfill_stable_ids` | give every SubItem lacking a `stable_id` one |
| `suggest_refs` | read-only: scan the document's `scope_paths` for matching functions/tests and propose refs per item (review, then `set_refs`) |

**On a layer document**, `add_item`, `set_priority`, `backfill_stable_ids`
and `set_refs` with `test_refs` are refused — those fields come from the
Markdown body. **`check` / `check_all` do not approve layer items:** on a
layer document they leave every SubItem's `status` / `approval` unchanged (and
return a warning), so one call can never approve items without a human
decision. Approve layer items with `trace_update`'s `set approval` instead
(`approved` records the item's current definition hash; `approved_hash` is
refreshed from the body at that moment). `sync` on a layer document runs the
same layer sync as `doc_save`: an item whose definition changed falls back to
`draft`.

Linking a task to a SubItem is done from the task side:
`handoff_update_task(task: {id, requirement_ids: [...]})`.

## `handoff_doc_verify_status`

`handoff_doc_verify_status(doc_id, include_items?: false, format?: "json" | "checklist")`
returns `{verification_status, progress: {checked, skipped, pending, total,
stale, percentage}}`; counts are per leaf (SubItems and freeform items).
`include_items` adds each item with a computed `stale` flag (its section
changed after it was checked). `format: "checklist"` renders Markdown for a PR
description or a human review:

```markdown
# Verification: Authentication Spec
Status: in_review (5/16, 31%)

## §2 1. Requirements ✓ verified ⚠ stale
- impl: src/auth.rs:42-180 (login)
- test: tests/auth.rs (roundtrip test)
- [ ] Lock after 5 failures
- [x] Login with password (@ai, 2026-07-11)

## — Drag-and-drop visual check ○ pending [visual]
```

## Workflow

| situation | do |
|---|---|
| spec just saved | `generate` (often `skip_seqs: [0]`) |
| a section is implemented | `check(fragment_seq: N, reviewer: "ai")` + `set_refs` |
| background-only section | `skip(fragment_seq: N)` |
| sections added/removed | `sync`, then re-check stale items |
| visual check by a person | `check(..., reviewer: "user")` after the user confirms |
| several requirements in one section | `add_item(fragment_seq: N, description: ...)` each, then check each SubItem |
| a check tied to no section | `add_item(label: ..., category: "visual")` |
| finding impl/test locations | `suggest_refs`, review, `set_refs` |
| readiness summary | `doc_verify_status(include_items: true, format: "checklist")` |
| release readiness across documents | `doc_list(doc_type: "spec", tags: [...])` → `doc_verify_status` each: all `verified`, `stale == 0` |

**Stale items**: when `doc_verify_status` shows `stale: true`, read the
section (`doc_get(format: "section", seq: N)`); if the implementation still
matches, `check` it again; otherwise fix the implementation first.

`reviewer: "ai"` — verified by reading code, running tests or comparing spec
and implementation. `reviewer: "user"` — confirmed by a person (GUI, layout,
judgment calls).

Example:

```
handoff_doc_save(slug: "auth-spec", title: "Authentication Spec",
                 body: "# Auth Spec\n\n## Requirements\n...", doc_type: "spec", task_ids: ["t42"])
handoff_doc_verify(doc_id: "doc-...", action: "generate", skip_seqs: [0])
handoff_doc_verify(doc_id: "doc-...", action: "check", fragment_seq: 1, reviewer: "ai", notes: "implemented and tested")
handoff_doc_verify(doc_id: "doc-...", action: "set_refs", fragment_seq: 1,
                   impl_refs: [{path: "src/auth.rs", lines: "10-50"}],
                   test_refs: [{path: "tests/auth.rs", label: "login flow"}])
handoff_doc_verify_status(doc_id: "doc-...", include_items: true)
```

## Importing requirements (`handoff_doc_req_import`)

Builds SubItems from a requirement-tree section (and optionally priorities
from a gap-analysis table) of an existing document. Dry-run first.

| Param | Default | Description |
|---|---|---|
| `doc_id` | — | Id or slug |
| `dry_run` | `true` | Preview only |
| `heading_pattern` | `要件ツリー` | Substring locating the requirement-tree section; if no heading contains it, the first heading starting with `2.` is used |
| `priority_source` | `gap_table` | `gap_table` (read priorities from a table), `manual`, or `none` |
| `gap_table_pattern` | `ギャップ分析` | Substring locating the gap-analysis section |
| `column_map` | — | `{id?, name?, priority?, status?}` — header text or 0-based column index, for table headers the built-in synonyms miss (e.g. `重要度`, `Pri.`, `項目`) |

- Each heading under the section becomes a SubItem with `stable_id`
  `<SLUG>-<heading id>` — e.g. heading `FR-001 Login` in a document with slug
  `auth-spec` becomes `AUTH-SPEC-FR-001`. Link tasks with that full id.
- Tables split across several numbered subsections are merged; rows match
  headings by ID first, then by fuzzy name; a merged cell like `P0=shipped`
  is split into `priority` and `dev_stage`.
- Check `parse_errors` in the dry run before writing.
- Refused on a layer document.

Then: `handoff_doc_req_status` to confirm counts.

## Querying requirements

| tool | parameters | returns |
|---|---|---|
| `handoff_doc_req_list` | `priority`, `dev_stage`, `category` (the `C<n>` stable_id prefix), `has_tests`, `task_id`, `sort` (`priority`/`category`/`dev_stage`/`stable_id`), `order`, `limit` (100), `offset` | every SubItem with a `stable_id` across all documents (layer items included), with `doc_id`, `doc_slug`, `task_ids` |
| `handoff_doc_req_status` | `tags`, `priority`, `category` | `by_status`, `by_priority`, `by_category`, `coverage {impl_pct, test_pct, verified_pct}` |
| `handoff_doc_req_scan` | `doc_id?`, `scope_paths?`, `patterns?` (`test_name` / `comment` / `symbol`) | ranked impl/test reference suggestions with confidence — never writes; apply with `doc_verify(set_refs)` |
| `handoff_doc_req_impact` | `file` or `git_diff: true` | requirements affected by a changed file (matched via `impl_refs`, `test_refs`, or the document's `scope_paths`) |

`doc_req_list` has no document filter — filter on `doc_slug` yourself.

## Diagnostics

| code | where | meaning | fix |
|---|---|---|---|
| `DIAG-R001` (warning) | `doc_req_*`, `load_context` | documents with no `layer` and no verification matrix — their requirements cannot be listed; `affected_doc_ids` lists them | `doc_save(doc_id, layer=...)` for V-model tracking, or `doc_verify(action: "generate")` for the section matrix. Holding imported req_* items? see `handoff-trace` → `references/migration.md` |
| `DIAG-R002` (info) | `doc_req_list` | SubItems without a `stable_id` are skipped | `doc_verify(doc_id, action: "backfill_stable_ids")` |
| `DIAG-D001` (warning) | `doc_save` | a `spec`/`design` document saved without a `layer` is outside V-model tracking | set a layer, enable `[trace] auto_layer`, or ignore if intentional |

`doc_req_scan` / `doc_req_impact` results are scoped by `scope_paths`: a
missing suggestion usually means the document has none set. An empty
`doc_req_list` / `doc_req_status` with a `DIAG-R001`/`R002` warning is a setup
issue, not "no requirements".
