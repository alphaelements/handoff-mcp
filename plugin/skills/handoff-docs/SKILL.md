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
2. `handoff_doc_save(title=..., body=..., doc_type="spec", task_ids=[...])`
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
| `title` | yes | Document title |
| `body` | yes | Full Markdown source — this is what gets split into fragments |
| `doc_type` | no | One of `spec`, `design`, `adr`, `guide`, `note` |
| `tags` | no | Free-form tags, folded into the BM25 index |
| `scope_paths` | no | Path prefixes this doc applies to — boosts relevance in `doc_query` when the matching file is being edited |
| `parent_id` | no | Places this document under a parent in the family tree |
| `related` | no | Array of `{ id, rel }` — semantic links to other documents (see Family Tree below) |
| `task_ids` | no | Task IDs to bidirectionally link (see Task Linking below) |
| `split_level` | no | ATX heading level to split on (default: `2`, i.e. `##`) |
| `auto_inject` | no | Injection hint: `auto` (default) \| `full` \| `outline` \| `none` |
| `doc_id` | no | Provide to update an existing document; omit to create a new one |

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
| `action` | yes | One of: `generate`, `check`, `check_all`, `skip`, `sync`, `set_refs`, `add_item`, `suggest_refs` |
| `fragment_seq` | for `check`/`skip`/`set_refs`/`add_item` | Section seq to operate on (integer or array of integers for batch). For `add_item`, omit to add a freeform top-level item instead of a section sub_item. |
| `sub_item_index` | no | For `check`/`skip`: the 0-based `SubItem.index` within `fragment_seq`'s `sub_items` to operate on, instead of the parent item itself (v2) |
| `description` | for `add_item` when `fragment_seq` given | The new sub_item's description (v2) |
| `label` | for `add_item` when `fragment_seq` omitted | The new freeform top-level item's label (v2) |
| `category` | no | For `add_item`: item/sub_item category — `"requirement"` (default for sub_items), `"visual"`, `"regression"`, `"manual"`, ... free-extensible (v2) |
| `skip_seqs` | for `generate` | Seqs to mark `skipped` on generation (e.g. `[0]` to skip the preamble) |
| `reviewer` | no | `"ai"` or `"user"` — who performed the review |
| `notes` | no | Free-text notes attached to the check |
| `impl_refs` | for `set_refs` | Array of `{ path, lines?, label? }` — implementation locations |
| `test_refs` | for `set_refs` | Array of `{ path, lines?, label? }` — test locations |

**Actions:**

| Action | What it does |
|---|---|
| `generate` | Create a new verification matrix from the document's sections. Errors if a matrix already exists (use `sync` to update). |
| `check` | Mark one or more sections (or, with `sub_item_index`, a single sub_item) as `verified`. Records `verified_at` and `content_hash_at_verify`. |
| `check_all` | Mark every section — and every sub_item (v2) — in the matrix as `verified` in one call. |
| `skip` | Mark a section (or, with `sub_item_index`, a single sub_item) as `skipped` (not applicable for review). |
| `sync` | Re-synchronize the matrix after sections changed (added/removed). Preserves existing item statuses; freeform items (v2) are never dropped. |
| `set_refs` | Attach `impl_refs` / `test_refs` to a section item. |
| `add_item` (v2) | With `fragment_seq`: append a `SubItem` (individual requirement) to that section's `sub_items` — `description` required. Without `fragment_seq`: append a freeform top-level item not tied to any section (e.g. a GUI check or regression test) — `label` required. |
| `suggest_refs` | Read-only. Scans the document's `scope_paths` for source/test files (`.rs`/`.ts`/`.tsx`/`.py`/`.go`/`.js`/`.jsx`) and fuzzy-matches `fn`/`struct`/`impl`/`mod` definitions and test functions (`#[test]`, `fn test_*`, files under `tests/`) against each item's heading, returning up to 20 `impl_refs`/`test_refs` candidates per item for review. Requires an existing matrix (`generate` first). Does not mutate the document — accept candidates by passing them to `set_refs`. |

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

`handoff_doc_save(task_ids: [...])` creates a **bidirectional** link:

1. The document's own `task_ids` field is set.
2. Each linked task gets a `TaskLink { target: doc_id, link_type: "doc", label: <doc title> }` entry in its `task_links`.
3. Deleting the document removes it from the linked tasks' `task_links` automatically.

Look up the relationship from either side:
- `handoff_doc_list(task_id: "T-79")` — documents linked to a task.
- `handoff_get_task(task_id: "T-79")` — inspect `task_links` on the task record to
  see which documents (and other targets) it links to.

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
