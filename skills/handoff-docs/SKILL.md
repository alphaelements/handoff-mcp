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

## The 9 Doc Tools

| Tool | Purpose |
|---|---|
| `handoff_doc_save` | Create or update a document. Splits `body` into fragments automatically. |
| `handoff_doc_get` | Read a document — `full` (reassembled body), `meta` (manifest only), or `fragment` (one section). |
| `handoff_doc_list` | List/search documents (BM25 over title + fragment bodies), filter by `doc_type`, `tags`, `task_id`. |
| `handoff_doc_delete` | Delete a document and all its fragments; unlinks it from any linked tasks. |
| `handoff_doc_reassemble` | Reconstruct the original Markdown from fragments, with drift detection. |
| `handoff_doc_tree` | Walk the family tree (ancestors/descendants/related) for a document. |
| `handoff_doc_query` | Context injection — hook-driven, staged `full`/`outline` results ranked by relevance. |
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

### `handoff_doc_tree`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Root of the traversal |
| `depth` | no | How many parent/child levels to return |
| `include_related` | no | Whether to also include semantically `related` documents |

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

Writes all `_doc.*.json` + `_frag.*.{json,md}` atomically in one transaction,
including any task links — matches the "validate whole tree, then write"
pattern used by `handoff_import_context` for tasks.

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
