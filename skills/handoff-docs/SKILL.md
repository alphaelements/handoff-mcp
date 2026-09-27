---
name: handoff-docs
description: "Document management — save, read, search, import, and traverse structured project documents (specs, designs, ADRs, guides, notes). Triggers on 'ドキュメント保存', '仕様書を管理', '設計書をインポート', 'save this doc', 'import specs', 'document management', 'タスク開始', 'spec registration', '仕様登録', 'verification check', or when the user asks to persist/organize/search multi-section markdown that is too structured for a single memory entry, or when the AI writes a spec/design document during development."
---

# Handoff Docs Skill

## When to use

- The user asks to save a spec, design doc, ADR, guide, or note so it survives across sessions
- The user wants to import an existing pile of Markdown files (e.g. a `wiki/` or `specs/` directory) into structured document management
- You need to read a specific section of a large document without pulling the whole thing into context
- You need to trace how documents relate to each other (parent/child hierarchy, or semantic links like "this design implements that spec")
- A document should be linked to one or more tasks so it surfaces automatically while that task is being worked on

If the knowledge is a short, standalone lesson/rule/convention/gotcha (< 1 page,
no internal sections), use `handoff-memory` instead — see "Memory vs Documents"
in `skills/handoff-memory/SKILL.md` for the boundary.

## Development Flow Integration

Document management is NOT just for explicit user requests. It activates
automatically during the standard development cycle:

### When writing a spec or design
After writing/updating a specification or finishing a `/design-review` session
(wiki/ or tmp/), immediately:
1. Start from a template (see "Templates" below) instead of a blank document.
2. `handoff_doc_save(slug="unique-spec-slug", title=..., body=..., doc_type="spec", task_ids=[...])`
3. `handoff_doc_verify(doc_id=..., action="generate")` to create the verification matrix

### When starting a task
Before implementation, fetch related specs:
- `handoff_doc_query(task_id="<task-id>")` — surfaces linked documents automatically
- Review the verification matrix: `handoff_doc_verify_status(doc_id=...)`

### When implementation is complete
Mark verified sections:
- `handoff_doc_verify(doc_id=..., action="check", fragment_seq=N)` for each completed section
- `handoff_doc_verify(doc_id=..., action="set_refs", fragment_seq=N, impl_refs=[...])` to record implementation locations

### When reviewing
Check readiness:
- `handoff_task_checklist(task_id=..., action="view")` — combined readiness view

### When importing requirements from a spec
After creating a spec document with `handoff_doc_save`:
1. `handoff_doc_verify(action="generate")` → build the verification matrix
2. `handoff_doc_req_import(doc_id="...", dry_run=true)` → preview SubItem generation
3. Review, then `handoff_doc_req_import(doc_id="...", dry_run=false)` → create SubItems
4. `handoff_doc_req_status` → verify counts

### When linking code to requirements (post-implementation)
After implementation is complete:
1. `handoff_doc_req_scan(scope_paths=["src/", "tests/"])` → auto-discover links
2. Review suggestions with confidence > 0.8
3. Apply: `handoff_doc_verify(action="set_refs", sub_item_id="...", impl_refs=[...])`

### When syncing test results
After running tests:
1. `cargo test --format json > test-results.json` (or the equivalent for the project's
   test runner)
2. `handoff_doc_req_test_sync(test_output_file="test-results.json")`
3. Check the matched/passed/failed summary in the response. A matched item on a
   **layer document** is recorded as a run (`handoff_trace_record`, under the
   hood) rather than written to `test_refs` — see
   [Recording execution results](#recording-execution-results-handoff_trace_record).

## Document Creation Rules

1. **One document = one complete document**
   - Always start with a `# Title` (h1 heading)
   - Group related content of the same category (ADR, spec, design, etc.) into a single document
   - Example: ADR-001 through ADR-005 belong inside `# Architecture Decision Records`
     as `## ADR-001: Redis Session` through `## ADR-005: ...` sections

2. **Use `append_body` to add sections**
   - When adding a section to an existing document, use `append_body` instead of rewriting the whole body
   - Example: `doc_save(doc_id="doc-...", append_body="## ADR-006: ...\n\n...")`

3. **MCP handles splitting internally**
   - There is no need for the AI to split content into separate documents manually
   - MCP automatically computes a section index at each h2 heading boundary
   - Use `doc_get(format="section", seq=N)` to retrieve individual sections on demand

4. **Choose the identifier for create or update**
   - To create a document, omit `doc_id` and provide an explicit, unique `slug`
     matching `[a-z0-9-]` (1–60 characters)
   - To update a document, provide its existing `doc_id`; its stored `slug` is
     retained and cannot be renamed with `handoff_doc_save`
   - Do not derive a missing slug from the title

## The 13 Doc Tools

| Tool | Purpose |
|---|---|
| `handoff_doc_save` | Create or update a document. Splits `body` into sections automatically. |
| `handoff_doc_get` | Read a document — `full` (reassembled body), `meta` (manifest only), or `section` (one section by seq). |
| `handoff_doc_list` | List/search documents (BM25 over title + section bodies), filter by `doc_type`, `tags`, `task_id`. |
| `handoff_doc_delete` | Delete a document; unlinks it from any linked tasks. |
| `handoff_doc_reassemble` | Reconstruct the original Markdown from sections, with drift detection. |
| `handoff_doc_update_section` | Replace a single section's content by seq (optimistic locking via `expected_hash`). |
| `handoff_doc_tree` | Walk the family tree (ancestors/descendants/related) for a document. |
| `handoff_doc_graph` | Visualize inter-document relationships; optionally includes verification status per node. |
| `handoff_doc_trace` | Trace a document's lineage or dependency chain. |
| `handoff_doc_query` | Context injection — hook-driven, staged `full`/`outline` results ranked by relevance. |
| `handoff_doc_verify` | Verification matrix operations: `generate`, `check`, `check_all`, `skip`, `sync`, `set_refs`, `add_item` (v2 — freeform items / sub_items), `suggest_refs` (scan scope_paths for impl/test ref candidates). |
| `handoff_doc_analyze` | Read-only heuristic scan of a file or directory — step 1 of the import flow. |
| `handoff_doc_import` | Atomic bulk write of analyzed + AI-reviewed documents — step 3 of the import flow. |

### `handoff_doc_save`

| Param | Required | Description |
|---|---|---|
| `slug` | when creating | Unique file-naming slug matching `[a-z0-9-]` (1–60 characters). Must be supplied explicitly; omitted on update because the existing slug is retained. |
| `title` | yes | Document title |
| `body` | yes | Full Markdown source — this is what gets split into fragments |
| `doc_type` | no | One of `spec`, `design`, `adr`, `guide`, `note` |
| `tags` | no | Free-form tags, folded into the BM25 index |
| `scope_paths` | no | Path prefixes this doc applies to — boosts relevance in `doc_query` when the matching file is being edited |
| `parent_id` | no | Places this document under a parent in the family tree |
| `related` | no | Array of `{ id, rel }` — semantic links to other documents (see Family Tree below) |
| `task_ids` | no | Task IDs to bidirectionally link (see Task Linking below) |
| `split_level` | no | ATX heading level to split on (default: `2`, i.e. `##`). On update, omitting this keeps the document's existing value — it does not reset to the default. |
| `auto_inject` | no | Injection hint: `auto` (default) \| `full` \| `outline` \| `none` |
| `layer` | no | V-model layer id: `requirement` \| `basic_spec` \| `detailed_spec` \| `acceptance` \| `system_test` \| `unit_test` (`[trace.id_prefixes]` in config only adds ID prefixes to these layers; custom layers are not supported yet, and an unknown id is treated as no layer). This is the only way to set a document's layer; omit to leave it untouched, pass `""` to clear it. Setting this turns the document into a **layer document**: every `doc_save`/`doc_update_section` call now parses the body for item headings and rebuilds the verification matrix from them — see "V-model Layer Documents" below. |
| `doc_id` | when updating | Existing document ID. Omit to create a new document; updates retain the existing document's slug. |

### `handoff_doc_get`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document to read |
| `format` | no | `full` (default-equivalent reassembly) \| `meta` (manifest only, no body) \| `fragment` |
| `seq` | when `format=fragment` | Fragment sequence number to return |

Use `meta` when you only need to walk the graph (titles, tags, relations)
without paying the token cost of fragment bodies. Use `fragment` with a `seq`
from an outline injection (see Staged Injection) to fetch exactly the section
you need.

### `handoff_doc_list`

| Param | Required | Description |
|---|---|---|
| `query` | no | BM25 search over title + fragment bodies |
| `doc_type` | no | Filter by type |
| `tags` | no | Filter by tags |
| `task_id` | no | Documents linked to this task |
| `include_body` | no | Default `false` — metadata-only listing |

### `handoff_doc_delete`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document to delete, along with all its fragments |

Deleting also removes the document from any linked task's `task_links`.

### `handoff_doc_reassemble`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document to reconstruct |
| `output_path` | no | If set, also writes the reassembled Markdown to this path |

Fragments are concatenated in `seq` order with original heading markers
preserved — `save(body) → reassemble()` is byte-identical. If a fragment was
edited directly after the split, its `content_hash` no longer matches and
`reassemble` reports the drift instead of silently returning stale content.

### `handoff_doc_update_section`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document to update |
| `seq` | yes | Section sequence number to replace |
| `new_content` | yes | New Markdown content for the section (empty string deletes it) |
| `expected_hash` | no | Optimistic lock — if set, the update fails when the section's current `content_hash` differs (returns the current hash so you can retry) |

Replaces a single section's content without rewriting the entire document.
The section's `content_hash` is recomputed after the update, and any
verification matrix item for this seq is marked stale.

### `handoff_doc_tree`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Root of the traversal |
| `depth` | no | How many parent/child levels to return |
| `include_related` | no | Whether to also include semantically `related` documents |

### `handoff_doc_graph`

| Param | Required | Description |
|---|---|---|
| `doc_id` | no | Focus on a specific document and its neighbors |
| `include_verification` | no | Include `{total, verified}` verification progress per node |

### `handoff_doc_trace`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document to trace from |
| `direction` | no | `"up"` (ancestors) or `"down"` (descendants), default both |

### `handoff_doc_query`

| Param | Required | Description |
|---|---|---|
| `text` | no | Prompt/query text (BM25 relevance ranking) |
| `file_paths` | no | Files being worked on — boosts documents whose `scope_paths` prefix-match |
| `task_id` | no | Boosts documents linked to this task |
| `session_id` | no | Enables per-session dedup — a fragment already injected this session is skipped unless its content changed |
| `limit` | no | Max fragments to return |
| `mark_injected` | no | Whether to record injection for dedup (default `true`) |
| `suppress_doc_ids` | no | Document ids to exclude entirely from this call's results |
| `suppress_until_changed` | no | With `suppress_doc_ids` and `session_id`: persists the suppression in the session sidecar so those documents stay excluded from future calls until their `content_hash` changes (default `false`) |

This is the hook-driven tool — see Staged Injection below for how results are shaped.

### `handoff_doc_analyze`

| Param | Required | Description |
|---|---|---|
| `path` | yes | File or directory to scan |
| `recursive` | no | Recurse into subdirectories |
| `flatten` | no | Skip hierarchy inference (no `parent_id` guessing) |

Read-only — writes nothing. Returns a conditioning report with
`auto_resolved` entries (high-confidence `doc_type`/`tags`/`scope_paths`) and
`needs_review` entries (broken links, missing relationships, near-duplicates)
each carrying a concrete `suggestion` the AI can approve, edit, or reject.

### `handoff_doc_import`

| Param | Required | Description |
|---|---|---|
| `analyzed` | yes | The `handoff_doc_analyze` output (possibly after AI review) |
| `overrides` | no | Per-file corrections (`doc_type`, relationship resolutions, etc.) |
| `task_ids` | no | Link every imported document to these tasks |

Writes all documents atomically in one transaction, including any task links
— matches the "validate whole tree, then write" pattern used by
`handoff_import_context` for tasks.

### `handoff_doc_verify`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document whose verification matrix to operate on |
| `action` | yes | One of: `generate`, `check`, `check_all`, `skip`, `sync`, `set_refs`, `set_dev_stage`, `set_priority`, `link_task`, `add_item`, `backfill_stable_ids`, `suggest_refs` |
| `fragment_seq` | for `check`/`skip`/`set_refs`/`set_dev_stage`/`set_priority`/`link_task`/`add_item` | Section seq to operate on (integer or array of integers for batch, `check` only). For `add_item`, omit to add a freeform top-level item instead of a section sub_item. For every SubItem-addressing action (`check`/`skip`/`set_refs`/`set_dev_stage`/`set_priority`/`link_task`), may be omitted when `sub_item_id` is given instead (FR-806) — see below. |
| `sub_item_id` | no | For `check`/`skip`/`set_refs`/`set_dev_stage`/`set_priority`/`link_task`: the SubItem's stable, immutable `stable_id` to address, instead of the parent item itself. When given, `fragment_seq` may be omitted (FR-806) — the SubItem is located by `stable_id` across every item in the matrix, including freeform ones (`fragment_seq: null`, e.g. from `add_item` with no `fragment_seq`, or from `handoff_doc_req_import`). `fragment_seq` is still required when addressing by `sub_item_index` instead, or when targeting a section item directly. Preferred over `sub_item_index` if both are given. |
| `sub_item_index` | no | For `check`/`skip`/`set_refs`/`set_dev_stage`/`set_priority`/`link_task`: the 0-based `SubItem.index` within `fragment_seq`'s `sub_items` to operate on, instead of the parent item itself (v2) |
| `description` | for `add_item` when `fragment_seq` given | The new sub_item's description (v2) |
| `label` | for `add_item` when `fragment_seq` omitted | The new freeform top-level item's label (v2) |
| `category` | no | For `add_item`: item/sub_item category — `"requirement"` (default for sub_items), `"visual"`, `"regression"`, `"manual"`, ... free-extensible (v2) |
| `skip_seqs` | for `generate` | Seqs to mark `skipped` on generation (e.g. `[0]` to skip the preamble) |
| `reviewer` | no | `"ai"` or `"user"` — who performed the review |
| `notes` | no | Free-text notes attached to the check |
| `impl_refs` | for `set_refs` | Array of `{ path, lines?, label? }` — implementation locations |
| `test_refs` | for `set_refs` | Array of `{ path, lines?, label? }` — test locations |
| `dev_stage` | for `set_dev_stage` | One of `not_started`/`in_progress`/`implemented`/`tested`/`verified` — the SubItem's implementation-progress stage (distinct from the verification-review `status` field) |
| `priority` | for `set_priority` | One of `P0`/`P1`/`P2`/`P3` |
| `task_ids` | for `link_task` | Array of task ids to link to this SubItem — **replaces** its existing `task_ids` wholesale (not a diff; also adds the reverse `{link_type:"requirement", label:stable_id}` entry on each linked task). Prefer `handoff_update_task(requirement_ids=...)` for incremental add/remove — see "Task Linking" below. |

**Actions:**

| Action | What it does |
|---|---|
| `generate` | Create a new verification matrix from the document's sections. Errors if a matrix already exists (use `sync` to update). |
| `check` | Mark one or more sections (or, with `sub_item_index`/`sub_item_id`, a single sub_item) as `verified`. Records `verified_at` and `content_hash_at_verify`. |
| `check_all` | Mark every section — and every sub_item (v2) — in the matrix as `verified` in one call. |
| `skip` | Mark a section (or, with `sub_item_index`/`sub_item_id`, a single sub_item) as `skipped` (not applicable for review). |
| `sync` | Re-synchronize the matrix after sections changed (added/removed). Preserves existing item statuses; freeform items (v2) are never dropped. **On a layer document**, this delegates entirely to the layer-body sync (same as `doc_save`/`doc_update_section` — see "V-model Layer Documents" below) instead of the plain per-section rebuild. |
| `set_refs` | Attach `impl_refs` / `test_refs` to a section item or SubItem. |
| `set_dev_stage` | Set a SubItem's `dev_stage` (`sub_item_id`/`sub_item_index` required — `dev_stage` is a SubItem-only field, not a section-level one). |
| `set_priority` | Set a SubItem's `priority` (`sub_item_id`/`sub_item_index` required). |
| `link_task` | Replace a SubItem's `task_ids` wholesale (`sub_item_id`/`sub_item_index` required) and add the reverse `task_links` entry on each linked task. A task id that doesn't resolve is a non-fatal warning. |
| `add_item` (v2) | With `fragment_seq`: append a `SubItem` (individual requirement) to that section's `sub_items` — `description` required. Without `fragment_seq`: append a freeform top-level item not tied to any section (e.g. a GUI check or regression test) — `label` required. |
| `backfill_stable_ids` | One-shot bulk backfill: mints a `stable_id` (via the same derivation `add_item` uses) for every SubItem across the whole matrix that doesn't have one yet; SubItems that already have one are left untouched. Takes only `doc_id` — no `fragment_seq`/`sub_item_id`. |
| `suggest_refs` | Read-only. Scans the document's `scope_paths` for source/test files (`.rs`/`.ts`/`.tsx`/`.py`/`.go`/`.js`/`.jsx`) and fuzzy-matches `fn`/`struct`/`impl`/`mod` definitions and test functions (`#[test]`, `fn test_*`, files under `tests/`) against each item's heading, returning up to 20 `impl_refs`/`test_refs` candidates per item for review. Requires an existing matrix (`generate` first). Does not mutate the document — accept candidates by passing them to `set_refs`. |

**M1 layer document guard applicability** (see "Layer document write guard"
immediately below): `add_item` / `set_priority` / `backfill_stable_ids` /
`set_refs` (only when `test_refs` is included) are **refused** on a document
with `layer` set. `set_dev_stage` / `link_task` / `check` / `check_all` /
`skip` / `sync` / `generate` / `suggest_refs` are **not** guarded — `sync` is
allowed but delegates to the layer-body sync instead of the plain rebuild
(see above), and the rest operate on runtime-only fields a layer document's
SubItems still track outside the body.

**Layer document write guard**: on a document with `layer` set, `add_item` /
`set_priority` / `set_refs` (only when the call includes `test_refs` —
`impl_refs`-only is still allowed) / `backfill_stable_ids` are **refused**
with an error telling you to edit the body instead — those fields are
defined by the Markdown body (`description`, `layer`, `refines`, `verifies`,
`method`, `priority`, `test_refs`), so a hand-authored SubItem on a layer
document would just be overwritten (as `origin: null`) or orphaned on the
next sync. `req_import` is refused outright on a layer document for the same
reason. `set_dev_stage` / `link_task` / `check` / `check_all` / `skip` — the
runtime fields a layer document's items still track — remain fully allowed.

### `handoff_doc_verify_status`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Document to query |
| `include_items` | no | `true` to include per-section item details (default `false` — summary only) |
| `format` | no | `"json"` (default) or `"checklist"` (v2 — Markdown checklist rendering, see below) |

Returns verification progress: `{ verification_status, progress: { checked, skipped, pending, total, stale, percentage } }`.
When `include_items: true`, also returns an `items` array with each section's
status, staleness flag, refs, reviewer, and notes (v2: including `category`,
`sub_items`, and `label` for freeform items). v2 progress counts are
leaf-based: an item with `sub_items` contributes its sub_items to
`checked`/`skipped`/`pending`/`total` instead of itself, and freeform items
(`fragment_seq: null`) are counted directly.

#### `format="checklist"` (v2)

`handoff_doc_verify_status(doc_id=..., include_items=true, format="checklist")`
returns a Markdown checklist instead of JSON — useful for pasting into a PR
description or presenting readiness to a human reviewer:

```markdown
# Verification: Document Title
Status: in_review (5/16, 31%)

## §2 1. Requirements ✓ verified ⚠ stale
- impl: src/storage/docs/mod.rs:42-180 (DocStore)
- test: tests/doc_save.rs (roundtrip test)
- [ ] Shape must be an octahedron
- [x] Color matches status (@ai, 2026-07-11)

## — Drag-and-drop visual check ○ pending [visual]
## — No layout regressions in the existing task list ○ pending [regression]
```

## Verification Workflow

### When to use each action

| Situation | What to do |
|---|---|
| Spec just saved | `generate` (optionally with `skip_seqs: [0]` to skip the preamble) |
| Implementation complete for a section | `check(fragment_seq=N, reviewer="ai")` + `set_refs` |
| Section is background/context only | `skip(fragment_seq=N)` |
| Spec was updated after matrix existed | `sync` to add/remove items, then re-check stale items |
| GUI/visual check needed | `check(fragment_seq=N, reviewer="user")` — user confirms manually |
| A section has multiple distinct requirements to track individually | `add_item(fragment_seq=N, description=...)` per requirement, then `check(fragment_seq=N, sub_item_index=I)` on each |
| A check doesn't map to any single section (GUI/regression/manual sweep) | `add_item(label=..., category="visual"\|"regression"\|"manual")` (freeform, no `fragment_seq`), then `check(fragment_seq=<its seq>)` |
| Human-readable readiness summary (PR description, review handoff) | `doc_verify_status(include_items=true, format="checklist")` — Markdown checklist |
| Quick release readiness | `doc_verify_status` — check `verification_status == "verified"` |
| Don't want to hunt for impl/test locations by hand | `suggest_refs` to get candidates per item, review them, then `set_refs(fragment_seq=N, impl_refs=..., test_refs=...)` with the ones you accept |

### `reviewer` guidelines

| Reviewer | When |
|---|---|
| `"ai"` | AI verified by reading the code, running tests, or comparing spec vs implementation |
| `"user"` | User verified visually (GUI, layout, drag behavior) or confirmed a judgment call |

### Stale detection and response

When a section's content changes after being verified, `doc_verify_status`
flags it as `stale: true`. Response flow:

1. Check `doc_verify_status(include_items: true)` — look for `stale` items
2. Review the changed section: `doc_get(format="section", seq=N)`
3. If still valid: `check(fragment_seq=N)` to re-verify (updates `content_hash_at_verify`)
4. If invalid: update implementation, then re-verify

### Multi-document release verification

For release readiness across multiple specs:

```
1. handoff_doc_list(doc_type="spec", tags=["release-target"])
2. For each doc: handoff_doc_verify_status(doc_id=...)
3. All docs must have verification_status == "verified" and stale == 0
```

### E2E workflow example

```
# 1. Write and save the spec
handoff_doc_save(slug="auth-spec", title="Authentication Spec",
                 body="# Auth Spec\n\n## Requirements\n...",
                 doc_type="spec", task_ids=["t42"])

# 2. Generate verification matrix (skip preamble)
handoff_doc_verify(doc_id="doc-...", action="generate", skip_seqs=[0])

# 3. Implement, then mark sections verified
handoff_doc_verify(doc_id="doc-...", action="check", fragment_seq=1,
                   reviewer="ai", notes="Implemented and tested")
handoff_doc_verify(doc_id="doc-...", action="set_refs", fragment_seq=1,
                   impl_refs=[{path: "src/auth.rs", lines: "10-50"}],
                   test_refs=[{path: "tests/auth.rs", label: "login flow"}])

# 3b. Or let suggest_refs propose candidates instead of hand-picking them —
#     requires scope_paths to be set on the document (doc_save(scope_paths=[...]))
handoff_doc_verify(doc_id="doc-...", action="suggest_refs")
# → { suggestions: [{ fragment_seq: 1, heading: "Requirements",
#      suggested_impl_refs: [{path: "src/auth.rs", lines: "12", label: "handle_login"}],
#      suggested_test_refs: [{path: "tests/auth.rs", lines: "5", label: "test_login_flow"}] }] }
# Review the candidates, then accept the ones you want via set_refs (same as 3).

# 4. Check release readiness
handoff_doc_verify_status(doc_id="doc-...", include_items=true)
# → verification_status: "verified", stale: 0 → ready to ship
```

## Staged Injection (outline vs full)

`handoff_doc_query` avoids flooding context with large documents:

| Mode | When | What's injected |
|---|---|---|
| `full` | Fragment body <= `doc_inline_threshold` tokens (default 300) | Metadata + the entire fragment body |
| `outline` | Fragment body > threshold | Metadata + heading list only — no body. Read a specific section with `handoff_doc_get(format="fragment", seq=N)` |

This means short documents (ADRs, conventions, short notes) get pulled in
ready-to-use, while long documents (full specs, design docs) only announce
their structure — the AI decides which section is actually worth fetching.

### `auto_inject` override

Set on `handoff_doc_save` (or later via an update) to force behavior
regardless of size:

| Value | Effect |
|---|---|
| `auto` (default) | Size-based automatic choice between `full`/`outline` |
| `full` | Always inject the full body |
| `outline` | Always inject headings only, even if small |
| `none` | Never auto-inject — only surfaced via explicit `handoff_doc_get` |

## Family Tree

Two distinct kinds of document relationship:

- **parent/child** (`parent_id`) — structural hierarchy, e.g. a directory of
  specs where each file's document is a child of the directory's document.
- **related** (`related: [{ id, rel }]`) — semantic links between documents
  that are not structurally nested. `rel` is one of:

  | `rel` | Meaning |
  |---|---|
  | `supersedes` | Replaces the target (a version bump) |
  | `references` | Loose cross-reference |
  | `implements` | This document implements the spec the target describes |
  | `extends` | Extends the target without replacing it |
  | `conflicts` | Known contradiction that needs resolving |

Use `handoff_doc_tree` to walk both kinds together (`include_related: true`)
or just the structural hierarchy.

## Task Linking

There are **two levels** of task linking. Use the right one for your purpose:

### Document-level links (coarse)

`handoff_doc_save(task_ids: [...])` creates a **bidirectional** link between
the **document as a whole** and one or more tasks:

1. The document's own `task_ids` field is set.
2. Each linked task gets a `TaskLink { target: doc_id, link_type: "doc", label: <doc title> }` entry in its `task_links`.
3. Deleting the document removes it from the linked tasks' `task_links` automatically.

This is useful for associating a spec document with its parent task, but it
does **not** link individual requirements (SubItems). The VSCode Requirements
Explorer does **not** read document-level `task_ids`.

### Requirement-level links (per SubItem — this is what Requirements Explorer shows)

To link a task to specific requirements (SubItems with a `stable_id`), use
**either** of these:

- `handoff_update_task(task={ id: "<task_id>", requirement_ids: ["FR-100", "NFR-060"] })`
  — on an existing task, this is a **diff against the task's current
  requirement_ids**: stable_ids newly present are added, previously-linked
  ones now absent are removed (both sides: `SubItem.task_ids` and the task's
  own `TaskLink{link_type:"requirement", label:"FR-100"}`). A stable_id being
  removed whose SubItem no longer resolves (its requirement item was deleted)
  still has its `task_links` entry unlinked, matched by `label`. On a new
  task, every id is added. Preferred for incremental linking.
- `handoff_doc_verify(doc_id, action="link_task", fragment_seq, sub_item_id, task_ids=[...])`
  — **replaces** a single SubItem's `task_ids` wholesale.

In the session-loop workflow, the manager calls `requirement_ids` automatically
when processing the developer's `### Requirements addressed` report. For
manual work outside session-loop, pass `requirement_ids` when creating or
updating a task that implements specific requirements.

#### `role`: implements vs. executes (wiki/220-vmodel-integration-design.md §2.5)

Every `requirement_ids` link also carries a `role`, `"implements"` (default)
or `"executes"`:

- `handoff_update_task(task={ id, requirement_ids: [...] }, requirement_roles: { "FR-100": "executes" })`
  sets an explicit role per stable_id. A stable_id in `requirement_ids` with no
  entry in `requirement_roles` has its role **inferred** from the linked
  SubItem's effective-layer side: right side (e.g. `system_test`/`unit_test`
  layers, `category: "check"`) infers `"executes"`; left side or no layer
  infers `"implements"`. Changing only the role of an already-linked,
  unchanged stable_id is handled as a role-only update (no SubItem mutation).
- Only `"implements"` links propagate this task's status changes to the
  linked requirement's `dev_stage` (`handoff_update_task(status=...)`).
  `"executes"` links (a test-execution task, e.g. one that runs a
  `unit_test`-layer item) never move `dev_stage` — a test task finishing does
  not mean the requirement it tests is implemented.
- Pre-M1 links (no `role` recorded) are treated as `"implements"` for
  backward compatibility.

### Repairing drifted `task_ids` (`handoff_doc_repair_task_ids`)

Every link-change path above (and layer sync) keeps `SubItem.task_ids` in
sync **differentially** — only the specific stable_ids one call actually
touches. If state ever drifts anyway (manual edits, an imported corpus, a
bug), `handoff_doc_repair_task_ids()` forces a full, all-tasks-scanning
rebuild of every requirement SubItem's `task_ids` from `TaskData.task_links`
(the source of truth) across the whole corpus. It is gated on the `tasks_*`
input fingerprint (§4.3): a call with no task changes since the last
full-corpus sync is a cheap no-op (`{ran: false}`), not a forced rescan. The
same gated full-rebuild function is also the self-repair hook
`handoff_trace_report` runs on every call (see "Coverage and gap reporting"
below). Takes no arguments beyond `project_dir`. Returns `{ran,
sub_items_changed, docs_changed}`.

### Lookup

- `handoff_doc_list(task_id: "T-79")` — documents linked to a task (document-level).
- `handoff_get_task(task_id: "T-79")` — inspect `task_links` on the task record to
  see which documents (and other targets) it links to. Links with
  `link_type: "requirement"` show per-SubItem links; `link_type: "doc"` show
  document-level links.

Note: there is no dedicated `doc_id` filter on `handoff_list_tasks` — the
`task_links` field is populated and readable per-task via `handoff_get_task`,
but a document → "which tasks link to me" listing must be done by scanning
`task_links` yourself, not via a built-in filter.

## Fragment Granularity

- Default split boundary: ATX heading level 2 (`##`).
- Override per-call with `split_level` on `handoff_doc_save` (e.g. `1` to
  split only on `#`, or `3` for finer-grained `###` sections).
- Content before the first qualifying heading becomes fragment `seq: 0` (the
  preamble). Nested headings below `split_level` stay inside their parent
  fragment rather than becoming their own fragment.

## Import Workflow (3 steps)

Use this instead of calling `doc_save` file-by-file when bringing in an
existing pile of Markdown — cross-document relationships and duplicates can
only be validated when every file is visible at once.

1. **`handoff_doc_analyze(path, recursive, flatten)`** — read-only scan.
   Returns `auto_resolved` (confident guesses) and `needs_review` (broken
   links, missing relationships, near-duplicates), each with a `suggestion`.
2. **AI reviews the report** — approve `auto_resolved` entries as-is, and for
   each `needs_review` item either accept the `suggestion`, correct it, or
   reject it. Nothing is written yet.
3. **`handoff_doc_import(analyzed, overrides, task_ids)`** — takes the
   analyzed payload plus the AI's overrides and writes the whole tree
   atomically, including task links.

## V-model Layer Documents (V字トレース)

A **layer document** is any document with `doc_save(layer=...)` set to one of
the 6 built-in V-model layers. Once set, its Markdown body — not
`doc_verify` calls — is the source of truth for its requirement/verification
items: every `doc_save`/`doc_update_section` call (and `doc_verify(sync)`)
re-parses the body and rebuilds the verification matrix from it.

### The 6 layers

| layer id | side | level | pairs with | default ID prefixes |
|---|---|---|---|---|
| `requirement` | left | 1 | `acceptance` | `REQ`, `FR`, `NFR` |
| `basic_spec` | left | 2 | `system_test` | `SPEC`, `BS` |
| `detailed_spec` | left | 3 | `unit_test` | `DS` |
| `acceptance` | right | 1 | `requirement` | `AT` |
| `system_test` | right | 2 | `basic_spec` | `ST` |
| `unit_test` | right | 3 | `detailed_spec` | `UT` |

`side: left` = definition (requirements/specs), `side: right` = verification.
Add project-specific prefixes without replacing the defaults via
`[trace.id_prefixes]` in `config.toml`:

```toml
[trace]
layers = ["requirement", "basic_spec", "acceptance", "system_test"]  # omit to auto-detect
[trace.id_prefixes]
requirement = ["UC"]
```

### Body syntax

A heading whose text **starts with an allowed ID** (`<PREFIX>-<digits>[letter]`,
e.g. `SPEC-012`, `AT-001a`, `ST-LOGIN-01`; `:`/`.` right after the ID is a
separator, not part of the title) is one item, regardless of heading level.
A heading that doesn't start with a recognized-but-unlisted prefix is just an
ordinary section heading; an ID-*looking* heading with a disallowed prefix
(e.g. `HTTP-2`) is silently left alone (with a warning in the response).

```markdown
### SPEC-012 ログイン失敗時のアカウントロック

- refines: REQ-003
- priority: P1

5回連続で認証に失敗したアカウントを 15 分間ロックする。

### ST-040 5回失敗でロックされる

- verifies: SPEC-012
- method: manual

手順: 誤パスワードで5回ログインする。
期待結果: 6回目は正しいパスワードでも拒否され、ロック中メッセージが出る。
```

Known attribute keys (the first contiguous bullet list right after the
heading only — a blank line before it is fine, but a blank line *inside*
breaks the block): `refines`, `verifies` (comma-separated IDs), `layer`
(per-item override), `priority` (`P0`-`P3`), `method`
(`manual`\|`auto`\|`visual`\|`review`), `test` (`path::name`, repeatable).
Everything else after the heading (unknown-key lines, later bullet blocks,
prose) is the item's body/statement. An item's effective layer is its own
`- layer:` override if present, else the document's `layer` — so one
document can mix a defining layer with its paired verification layer (as in
the example above: `basic_spec` doc with an inline `system_test` item).

### Layer document templates

Four starter bodies, one per commonly-used layer (wiki/220 §6) — start a new
layer document from the matching template's shape rather than inventing
heading syntax from scratch. Each is a few lines: an `#` title, one item
heading whose text starts with that layer's default ID prefix, its known
attribute lines, and one line of statement/body.

**Requirement** (`layer="requirement"`, left, pairs with `acceptance`):

```markdown
# <Feature> Requirements

### FR-001 <short requirement title>

- priority: P1

<One or two sentences of the actual requirement statement.>
```

**Basic spec** (`layer="basic_spec"`, left, pairs with `system_test`):

```markdown
# <Feature> Basic Spec

### SPEC-001 <short spec item title>

- refines: FR-001

<How this requirement is realized at the design level.>
```

**Acceptance** (`layer="acceptance"`, right, verifies `requirement`):

```markdown
# <Feature> Acceptance Tests

### AT-001 <short check title>

- verifies: FR-001
- method: manual

手順: <steps to reproduce>.
期待結果: <expected outcome>.
```

**System test** (`layer="system_test"`, right, verifies `basic_spec`):

```markdown
# <Feature> System Tests

### ST-001 <short check title>

- verifies: SPEC-001
- method: auto
- test: tests/<file>.rs::<test_fn>

<What the automated test asserts.>
```

Save each with `handoff_doc_save(slug=..., title=..., layer=..., body=...)` —
`layer` is what turns the document into a layer document (see "The 6 layers"
above); everything else is ordinary `doc_save`.

### Minimal configurations

- **Requirement + acceptance only**: two documents (`layer="requirement"`,
  `layer="acceptance"`), acceptance items `verifies:` the requirement ids.
- **Requirement + inline verification (layer-skip)**: a single
  `layer="requirement"` document where an item carries its own `test:` or
  `method:` attribute — that item is *both* the requirement and its own
  verification item (no separate acceptance/system_test document needed).
  This is the fastest way to close the V without writing a second document.

### Category and aggregation

Layer sync sets each parsed item's `SubItem.category` from its effective
layer's side: `side: right` -> `"check"` (a verification item), `side: left`
-> `"requirement"`. `aggregate_requirements` (and therefore
`_requirements_summary.json`, `handoff_doc_req_status`) **excludes**
`category == "check"` items from every count (`total`, `by_status`,
`by_priority`, `by_category`, `coverage`, `task_coverage`) — they are
verification items, not requirements to be implemented — but still lists
them in `items[]` (with `category`/`layer`) so a caller can render them.

### Editing a layer document

Edit the body and re-`doc_save` (or `doc_update_section`) it — never
`doc_verify(add_item/set_priority/set_refs(test_refs)/backfill_stable_ids)`
or `req_import` (see the write guard note under `handoff_doc_verify` above).
Allowed through `doc_verify`: `set_dev_stage`, `link_task`, `check`,
`check_all`, `skip` — the runtime fields (implementation progress, task
links, review status) a layer item still tracks outside the body. If you
hand-edit the `.md` file directly (bypassing `doc_save`), the next
`doc_save`/`doc_update_section`/`doc_verify(sync)` call picks the edit up
automatically: items removed from the body are dropped (reported as a
`removed: [...]` warning), and edited titles/attributes update in place —
`sub_item.stable_id` never changes as long as the heading's ID doesn't.

A re-sync is skipped only when nothing that could change the matrix's shape
actually changed: the body's raw bytes are byte-identical to what was last
synced, **and** neither `layer` nor `split_level` changed on this call — a
metadata-only `doc_save` that changes only `layer` (e.g. moving a document
from `basic_spec` to `system_test`) or only `split_level` still forces a
full re-sync, since either one changes an item's `category` or which
section it belongs to even though not a single body byte moved.

Whenever a re-sync actually runs, it also refreshes
`_requirements_summary.json` from every document (not just this one) and
warns if the id it just (re)assigned collides with a `stable_id` already
used elsewhere — either in a *different* document, or on a different
`SubItem` **within this same document** (most often a leftover
`req_import`/`add_item`-authored item from before `layer` was set). Either
kind of collision leaves the id ambiguous for `resolve_stable_ids` (and
therefore unlinkable via `handoff_update_task(requirement_ids=...)`) until
you resolve it by editing the body or the older item.

### AI layer-skip development flow

1. Write a `basic_spec` item with a `test:` attribute (inline verification).
2. `handoff_update_task(task={id, requirement_ids: [stable_id, ...]})` to
   link your task to it.
3. Implement, run the test.
4. Record the result: `handoff_trace_record` (see below), or
   `set_dev_stage`/`check` through `doc_verify` for a manual review pass.

### Recording execution results (`handoff_trace_record`)

M1 (t360.8, wiki/220-vmodel-integration-design.md §2.6). Records one
execution batch — a set of `{item, result}` pairs from a single CI run or
manual verification pass — as one new file under `.handoff/runs/`
(`create_new`, never overwritten) and refreshes the derived
`runs/_latest.json` cache (each item's most recent result).

```
handoff_trace_record(results=[
  {item: "ST-040", result: "pass", note: "ran locally", evidence: ["tests/e2e.rs::lockout"]}
])
```

- `result` is one of `pass` \| `fail` \| `blocked` \| `not_run` \| `skipped`.
- `body_hash` is never supplied by the caller — the tool fills it in from the
  matching SubItem's current `body_hash` automatically (M2 "suspect"
  detection: was this `pass` recorded against the item's *current*
  definition, or a since-edited one?).
- An `item` stable_id that doesn't resolve to any SubItem is still recorded
  (not rejected) — a warning naming it is returned instead.
- `commit` defaults to `git rev-parse --short HEAD` in `project_dir` (empty
  string if that fails); `executor_kind` defaults to `"ai"`.
- This is the layer-item counterpart of `handoff_doc_verify(set_refs)`'s
  `test_refs` for non-layer items — `test_refs` is body-owned once a
  document has a `layer` (see "Editing a layer document" above), so a layer
  item's test result belongs in a run, not a `CodeRef` label.
- `handoff_doc_req_test_sync` already calls this internally for every
  matched test result on a layer item, batched into one run per
  `req_test_sync` call — you don't need to call `handoff_trace_record`
  yourself when driving results through `req_test_sync`.
- Does **not** rebuild `.handoff/docs/_trace_report.json` itself (t360.13:
  measured ~271ms once tried, vs. this op's own ~100ms budget — recording a
  result is meant to stay cheap). Call `handoff_trace_report` (or CLI `trace
  report`) afterward to refresh the derived file; a stale one is detectable
  via its `inputs` fingerprint.
### Coverage and gap reporting (`handoff_trace_report`)

M1 (t360.10, wiki/220-vmodel-integration-design.md §3.2). Builds one
derivation graph from every layer document, task<->requirement link, and the
`runs/_latest.json` cache. Before aggregating, it also (a) re-syncs any layer
document whose body was edited **directly** on disk since its last sync (raw
FNV-1a byte hash mismatch — same trigger as an ordinary `doc_save`, so a hand
edit is picked up without needing to re-`doc_save`) and (b) self-repairs any
`SubItem.task_ids` drift (same gated full rebuild as
`handoff_doc_repair_task_ids`, above — a no-op unless task links changed).
These are the tool's only side effects.

```
handoff_trace_report(layers?: [string], gap_kinds?: [string], limit?: 50, include_items?: false)
-> {trace_layers: {in_use, source: "config"|"auto"}, coverage: {<layer>: {total, horizontal: {covered,uncovered,na}, vertical: {covered,uncovered,na}, state: {passing,failing,blocked,not_run,uncovered}}}, gaps: [{kind, item, layer, detail}], gap_counts: {<kind>: count}, warnings, items?}
```

- `layers` overrides `[trace] layers` config for this call only (empty/omit
  falls back to config, then auto-detection from which layers actually have
  items).
- `gap_kinds` restricts the `gaps[]` **list** to the given kinds
  (`unverified`\|`unrefined`\|`orphan`\|`dangling`\|`invalid_link`\|`cycle`\|
  `duplicate_id`\|`task_unlinked`) — `gap_counts` always reports every kind's
  total regardless of this filter.
- `limit` (default 50) truncates `gaps[]` after kind-filtering; a truncation
  is noted in `warnings`.
- `include_items: true` adds an `items[]` array (`id, layer, side, title,
  state, refines, verifies, tasks: [{id, role}], doc, seq, sub_item_index,
  priority, dev_stage, category, impl_refs, test_refs, last_run?`) to *this
  call's own response*.
- Every call without a `layers` override also (re)writes
  `.handoff/docs/_trace_report.json` (t360.13,
  wiki/220 §3.4) — the same shape as `include_items=true`'s `items[]` plus
  `schema_version`/`trace_layers`/`coverage`/`gaps`/`gap_counts` (unfiltered
  by this call's own `gap_kinds`/`limit`) and an `inputs` freshness
  fingerprint, for handoff-vscode's V-model view to read directly instead of
  re-implementing the derivation engine in TypeScript. Unformatted JSON, only
  actually rewritten when its content differs from what's on disk.
  `handoff_trace_record` does **not** also refresh this file (measured too
  expensive for that op's own budget — see below); call `handoff_trace_report`
  (or CLI `trace report`) after recording results to bring it up to date.
  A call with a non-empty `layers` override answers from the overridden
  layer set but leaves the file untouched (its `inputs` fingerprint cannot
  record the override, so persisting it would look like a fresh canonical
  report).

### Progressive-disclosure neighborhood view (`handoff_trace_slice`)

M1 (t360.11, wiki/220-vmodel-integration-design.md §3.3, FR-701). Same graph
as `handoff_trace_report` (and the same layer-doc resync side effect), but
returns only the neighborhood around one task or item — saves AI context
compared to a full report.

```
handoff_trace_slice(task_id? | item?, direction?: "both", depth?, expand?: [string], max_items?: 30)
-> {items: [{id, layer, side, title, state, refines, verifies, tasks: [{id, role}], statement?}], truncated, warnings}
```

- Exactly one of `task_id`/`item` is required. `task_id`'s starting set is
  every stable_id that task has a `requirement` link to (any role). An
  unknown `task_id` or `item` is an error, not an empty `{items: [],
  truncated: false}` result — every real item always includes at least
  itself, so an empty result for an `item` would otherwise be misread as
  "no trace neighborhood". (An existing `task_id` with no requirement links
  does return an empty `items[]` — that is a real answer, not an error.)
- `direction` (default `"both"`): `"up"` follows `refines` toward upper
  left-side items plus `verifies` toward the left-side item being verified;
  `"down"` follows `refines` toward refining children plus the verifiers that
  verify this item; `"both"` is the **union of the two one-way walks** from
  the same starting set — not a single traversal that explores both
  directions from every visited node (which could turn around at a parent
  and pull in unrelated sibling subtrees).
- `depth` (default unlimited) caps each one-way walk; a graph cycle always
  stops traversal via its own visited set regardless of `depth`.
- Every item defaults to `id`/`layer`/`side`/`title`/`state`/`refines`/
  `verifies`/`tasks` only — pass `expand: [stable_id, ...]` to also get
  `statement` (re-extracted from that item's current layer-document body) for
  just those ids.
- `max_items` (default 30) caps `items[]`; when the full reachable set is
  larger, the farthest-reached items are dropped first and `truncated: true`.
  Only ids that resolve to a real item count toward `max_items` — a dangling
  id (e.g. a task's requirement link whose owning document was deleted) is
  never returned and never consumes a slot.
- `warnings[]` carries the layer-doc resync's warnings (e.g.
  `removed: [ids]` after a direct `.md` edit dropped an item) — the resync
  has already been persisted, so this response is the only place they show.

### Execution history (`handoff_trace_history`)

t360.13 (wiki/220-vmodel-integration-design.md §3.4, VSCode FR-903's
execution-history display). Pure read over `.handoff/runs/` — never writes
anything.

```
handoff_trace_history(item: string, limit?: 20)
-> {items: [{run_id, executed_at, executor: {kind, id?}, result, note, evidence, commit}]}
```

Every recorded result for `item`, newest first by `(executed_at, run_id)`.

### CLI: `trace report` / `record` / `slice` / `history`

t360.13 (wiki/220 §3.4). The same four tools above, callable without an MCP
client — handoff-vscode spawns the native `handoff-mcp` binary directly (no
shell), so these run without a Node wrapper or shell interpreter in the way:

```
handoff-mcp trace report [--project-dir P] [--layers a,b] [--gap-kinds k1,k2] [--limit 50] [--include-items true]
handoff-mcp trace record --results '[{"item":"ST-1","result":"pass"}]' [--task-id T] [--executor-kind human]
handoff-mcp trace slice (--task-id T | --item ID) [--direction both] [--depth 2] [--max-items 30] [--expand a,b]
handoff-mcp trace history --item ID [--limit 20]
```

`trace report` is what regenerates `.handoff/docs/_trace_report.json` from a
cold CLI process (e.g. handoff-vscode detecting a stale `inputs` fingerprint
and re-running it) — measured well under PR-7's 1s budget even on a fresh
process against a 2,500-item/30-document corpus (see t360.13's dev report for
the numbers).

## `doc_type` Values

`spec` (requirements/behavior contracts) · `design` (architecture/design
docs) · `adr` (architecture decision records) · `guide` (how-to/operational
docs) · `note` (fallback — anything that doesn't fit the above).

## Templates

Three starter templates are registered as `doc_type="guide"` documents tagged
`template` (plus a type-specific tag: `spec`, `design`, or `adr`). Fetch one
with `handoff_doc_list(tags=["template"])` or `handoff_doc_get(doc_id=...)`
before writing a new spec/design/ADR from scratch — copy its section
structure rather than reinventing it:

| Template | Tags | Structure |
|---|---|---|
| Specification Template (`specification-template`) | `template`, `spec` | 課題 / ゴール / 設計 / 実装計画 / 検証チェックリスト / 未決事項 |
| Design Document Template (`design-doc-template`) | `template`, `design` | 概要 / 制約・前提 / 設計案（採用案・代替案）/ トレードオフ表 / 実装影響範囲 / リスク |
| ADR Template (`adr-template`) | `template`, `adr` | コンテキスト / 決定 / 理由 / 結果 |

Templates are registered with `auto_inject="none"` — they are reference
material fetched on demand, not injected into every prompt.

## Memory vs Documents

| Criterion | Use Memory | Use Documents |
|---|---|---|
| Size | < 1 page, no sections | Multi-section, structured |
| Lifecycle | Permanent lesson/rule | Versioned with the project |
| Granularity | Single fact/convention | Sections need independent tracking |
| Review tracking | Not needed | Verification matrix tracks per-section |
| Task linkage | Not applicable | Bidirectional task_ids |
| Example | "Always use SSH for git push" | "Authentication spec with 5 sections" |
