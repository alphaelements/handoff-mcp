# Migration guide

Two situations:

1. [Moving req_* requirements into a V-model layer document](#1-moving-req_-requirements-into-a-v-model-layer-document)
   — you imported requirements with `handoff_doc_req_import` and now want
   lint / suspect / coverage / tree tracking on them.
2. [Upgrading a project that already used layer documents](#2-upgrading-a-project-that-already-used-layer-documents)
   — picking up change tracking (link baselines) and newer item fields.

## 1. Moving req_* requirements into a V-model layer document

### Two representations, not interchangeable in place

| | req_* SubItems (`doc_req_import`) | layer document items |
|---|---|---|
| source of truth | the document's verification matrix | the Markdown body (`### FR-001 ...` headings) |
| stable IDs | `<SLUG>-<heading id>` (e.g. `AUTH-SPEC-FR-001` for slug `auth-spec`) | exactly the heading ID (`FR-001`) |
| edited with | `doc_verify(add_item / set_priority / set_refs / backfill_stable_ids)`, `doc_req_import` | the body (`doc_save`, `doc_update_section`, `trace_update(upsert_item)`) |
| trace tools (`trace_report`, `trace_lint`, `trace_tree`, suspects) | invisible (the document has no layer) | full support |

Once a document has a `layer`, `doc_req_import` is refused on it, and so are
`doc_verify`'s `add_item`, `set_priority`, `backfill_stable_ids` and
`set_refs` with `test_refs`.

> **Do not convert in place.** Setting `layer` on the document that holds the
> imported req_* SubItems does **not** replace them: the old SubItems
> (`AUTH-SPEC-FR-001`) stay next to the new body items (`FR-001`), every
> requirement is counted twice, and existing task links keep pointing at the
> old IDs. Write a new layer document and retire the old one instead.

### Procedure

1. **Inventory.** List what the old document holds and which tasks link to it:
   ```
   handoff_doc_req_list()                       # every req_* item: stable_id, title, priority, dev_stage, test_refs, task_ids
   handoff_doc_get(doc_id: "<old doc id>", format: "meta")   # the document's own verification matrix
   ```
   (`doc_req_list` has no document filter — pick the rows whose `doc_slug` is
   the old document.)
2. **Choose layer and profile.** Most imported requirement lists belong in
   `layer: "requirement"`; design-level lists in `"basic_spec"`. Make sure
   `[trace] profile` is set (see `config.md`).
3. **Write the new layer document.** One heading per old SubItem, in the
   heading → attribute block → body order (templates: the
   `handoff-requirements` skill). Carry over:

   | old field | new place |
   |---|---|
   | title | heading title |
   | priority | `- priority: P1` attribute |
   | test_refs | `- test: <test name>` attribute (the name the runner reports, not a file path) |
   | dev_stage | not a body attribute — it follows linked tasks; or set it with `trace_update(ops: [{op: "set", item, dev_stage}])` |
   | impl_refs | `trace_update(ops: [{op: "set", item, impl_refs: [...]}])` |

   Use plain heading IDs (`FR-001`), not the old slug-prefixed IDs — they
   would need the slug registered as an ID prefix.
   ```
   handoff_doc_save(slug: "auth-requirements", title: "Auth Requirements",
                    layer: "requirement", body: "# Auth Requirements\n\n### FR-001 ...")
   ```
4. **Re-link tasks.** For every task that linked an old ID, pass the new IDs.
   `requirement_ids` replaces the task's whole set, so the old link is removed
   in the same call:
   ```
   handoff_update_task(task: {id: "t12", requirement_ids: ["FR-002"]})
   ```
5. **Retire the old document** once nothing points at it:
   `handoff_doc_delete(doc_id: "<old doc id>")`. If the document also holds
   prose you still need, move that prose into the new document (or another
   document) first.
6. **Verify.** `handoff_doc_req_list()` shows only the new IDs, each task's
   `handoff_get_task` shows a `trace` field, and `handoff_trace_lint()` has no
   `dangling` / `task_link_dangling` findings. Expect `leaf_without_acceptance`
   warnings until you add acceptance criteria — that is the point of the move.

Queries keep working: layer items appear in `doc_req_list` / `doc_req_status`
alongside freeform ones.

### When not to migrate

A one-off spec whose requirements you only need to list and tick off does not
need a layer — `doc_req_import` + `doc_req_status` is enough. Layers pay off
when items must stay linked, verified and re-checked as they change.

## 2. Upgrading a project that already used layer documents

Newer releases only add optional fields (definition hashes, link baselines,
approval, `needs`, `assignee`, tree attributes). Existing documents open
unchanged; nothing is rewritten until the next write. To start change
tracking cleanly:

1. **Resync once.** Any of `handoff_doc_save`, `handoff_trace_report` or
   `handoff_trace_slice` re-parses layer documents and fills in the new
   per-item fields. Suspect state of existing links does not change.
2. **Preview baselines.**
   `handoff_trace_suspect(action: "baseline", dry_run: true)` counts
   `refines`/`verifies` links and task links that have no recorded baseline
   (`unbaselined`). Existing links are never backfilled silently.
3. **Record baselines.**
   `handoff_trace_suspect(action: "baseline", dry_run: false)` records the
   current definitions as the baseline. From now on, editing an upstream item
   makes its downstream links and tasks **suspect**, and the `unbaselined`
   lint finding disappears.

**Mixed binary versions.** If two `handoff-mcp` versions write the same
`.handoff/` (for example two worktrees, or a CI runner not yet upgraded), the
older one drops fields it does not know when it rewrites a layer document.
The newer binary re-derives most of them on its next sync, but a dropped
baseline is never restored silently — those links become `unbaselined` again
until step 3 is repeated. Upgrade every worktree and runner together before
relying on suspect tracking.

## Removed tools and actions

| removed | use instead |
|---|---|
| `handoff_doc_verify(action: "link_task")` | `handoff_update_task(task: {id, requirement_ids: [...]})` |
| `handoff_task_checklist(action: "generate")` | `handoff_trace_scaffold` |
| `handoff_doc_req_test_sync` | `handoff_trace_ingest(format: "cargo_json")` |
