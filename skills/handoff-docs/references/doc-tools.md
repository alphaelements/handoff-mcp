# Document tool reference

Parameters and behaviour of the generic `handoff_doc_*` tools. The
verification-matrix and `doc_req_*` tools are in
[verification-matrix.md](verification-matrix.md); V-model layer documents are
covered by the `handoff-trace` and `handoff-requirements` skills.

**Identifying a document.** Most tools take `doc_id` as either the document id
(`doc-20261017-...`) or its slug. **Exception: `handoff_doc_save` needs the
real id in `doc_id`** — a slug there fails with "Document not found". Get the
id from the create response or `handoff_doc_list`.

## `handoff_doc_save`

| Param | Required | Description |
|---|---|---|
| `slug` | when creating | Unique file-naming slug, `[a-z0-9-]`, 1–60 characters. Supply it explicitly (do not derive it from the title). Ignored on update — slugs cannot be renamed. |
| `doc_id` | when updating | Existing document **id** (not slug). Omit to create. |
| `title` | when creating | Document title. Optional on update/append (keeps the existing title). |
| `body` | when creating | Full Markdown, starting with a `# Title`. Mutually exclusive with `append_body`. Omit both on update for a metadata-only change (body kept verbatim). |
| `append_body` | no | New section(s) appended to an existing document's body. Requires `doc_id`. |
| `separator` | no | `append_body` only: text between the old body and the appended part (default a blank line). |
| `doc_type` | no | `spec` \| `design` \| `plan` \| `adr` \| `guide` \| `note`. Inferred on create when omitted (see SKILL.md). |
| `tags` | no | Free-form tags (indexed for search). |
| `scope_paths` | no | Path prefixes this document applies to — boosts relevance in `doc_query` / `doc_list(query)` when a matching file is in play. |
| `parent_id` | no | Parent document (family tree). |
| `related` | no | `[{id, rel}]`, `rel` ∈ `supersedes` / `references` / `implements` / `extends` / `conflicts`. |
| `task_ids` | no | Tasks to link bidirectionally at **document** level. On update, ids removed are unlinked. Unresolvable ids come back in `warnings`. |
| `split_level` | no | Heading level that starts a section (default `2`). On update, omitting it keeps the stored value. |
| `auto_inject` | no | `auto` (default) \| `full` \| `outline` \| `none`. |
| `layer` | no | V-model layer id (`requirement`, `basic_spec`, `detailed_spec`, `acceptance`, `system_test`, `unit_test`, or a custom `[[trace.layer]]` id). Turns the document into a layer document. Omit to keep, `""` to clear (clearing deletes nothing). See the `handoff-requirements` skill. |
| `trace_profile` | no | Per-document V-model profile override (`minimal` / `standard` / `full` / `bugfix` / custom). `""` clears it. |

Response: `{doc_id, slug, title, doc_type, section_count, content_hash,
warnings}` plus, when relevant, `scaffold_suggestions`, `schedule_stale`,
`orphaned_tasks` (see SKILL.md "Plan lifecycle notifications"), and for layer
documents `suspect_introduced`, `orphaned_children`, `errors[]`.

## `handoff_doc_get`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Id or slug |
| `format` | no | `full` (default: original body + metadata) \| `meta` (metadata only — cheap for graph walks) \| `section` (one section's body) |
| `seq` | when `format="section"` | Section number (from `meta`, an outline injection, or `doc_list`) |

## `handoff_doc_list`

| Param | Required | Description |
|---|---|---|
| `query` | no | BM25 ranking over title + tags + body, applied after filters |
| `doc_type` | no | Filter |
| `tags` | no | Filter (AND — every tag must be present) |
| `task_id` | no | Documents linked to this task (document-level links) |
| `include_body` | no | Default `false` |

`unreadable: [{slug, error, line?}]` lists any `_doc.<slug>.md` whose
frontmatter failed to parse — fix with `handoff_doc_repair_frontmatter`.

## `handoff_doc_delete`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Id or slug |

Removes the document and its body file, unlinks it from tasks' `task_links`,
removes it from its parent's children and clears `parent_id` on its own
children (no cascade). An unresolvable linked task is reported in `warnings`.
`doc_save(layer: "")` does **not** delete anything — it only clears the layer.

## `handoff_doc_update_section`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Id or slug |
| `seq` | yes | Current section number |
| `new_content` | yes | Replacement Markdown **including the section's heading line**; `""` deletes the section |
| `expected_hash` | no | Optimistic lock — must equal the section's current `content_hash`, else the call fails with the current hash and changes nothing |

On a layer document this re-parses the body like `doc_save`.

## `handoff_doc_reassemble`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Id or slug |
| `output_path` | no | Also write the body to this path |

Returns `{doc_id, body, drifted, output_path?}` — `drifted: true` when the body
file was edited outside `doc_save` (its hash no longer matches the recorded
one).

## `handoff_doc_export`

Renders documents as PDF. Release binaries compile with the Typst built into
them (nothing to install); a binary built without the `bundled-typst` feature
(plain `cargo build`) needs the Typst CLI (>= 0.12) and fails with install
instructions when it is missing. mmdc/Chrome stay optional for Mermaid diagrams.

| Param | Required | Description |
|---|---|---|
| `doc_id` | one of | Id or slug of one document |
| `doc_ids` | one of | Several ids/slugs; one PDF each (or one merged PDF with `bundle`). Mutually exclusive with `doc_id` |
| `task_id` | one of | Export the documents linked to this task as ONE merged PDF (REQ-PDF-027, see below). Mutually exclusive with `doc_id` / `doc_ids` / `doc_tree`; may be combined with `scope` |
| `format` | no | Only `pdf` (default) |
| `output_dir` | no | Put the PDF here instead of `.handoff/exports/` (relative to the project, no `..`; or absolute) |
| `template` | no | `default` / `report` / `spec` (in `.handoff/templates/`, editable) or a `.typ` path |
| `include_toc` | no | Force the table of contents on/off; omitted = the template's default |
| `bundle` | no | With `doc_ids`: merge them into ONE PDF (default `false`). Needs `doc_ids`, not `doc_id` |
| `bundle_title` | no | Title on the bundle cover (only with `bundle`; default "Document Bundle"); also names the file `bundle-<title>.pdf` |
| `page_break_style` | no | Only with `bundle`: how documents are separated. `break` (default) = each starts on a new page; `section` = a divider page with only the title, content on the next page; `none` = no page break, documents follow each other |
| `header` | no | Only with `bundle`: object `{show_title?, show_page_number?, show_date?}` (each default `true`; `{}` = all three). Replaces the template's header/footer on the document pages: header = current document's title (left) + date (right), footer = page number. Cover and contents keep the template's. Omitted = the template's own header/footer |
| `cover_version` | no | Version label on the cover page (e.g. `v1.2`); documents store no version, so the caller supplies it. Applies to every PDF of the call and to the bundle cover |
| `cover_status` | no | Lifecycle status on the cover page (e.g. `Draft`, `Approved`) |
| `cover_organization` | no | Issuing organization on the cover page |
| `enrich` | no | Default `true`: draw a meta block under each requirement item's heading (REQ-PDF-030, see below). `false` = plain export |

Returns `{format, exported, failed, pdf_path?, pdf_paths, documents, warnings}`
(`pdf_path` only for the `doc_id` form). Missing images, unrenderable Mermaid
blocks and similar problems are `warnings`, not failures; a Typst compile error
fails that document with Typst's diagnostics. In a batch, failed documents are
listed in `documents[]` with an `error` and the rest still export.

Bundle mode (`bundle: true`): one PDF with a cover (bundle title plus the
numbered document list), one table of contents covering every document's
sections, and each document on a new page under its own title as a level-1
heading (its own headings sit below it, the shallowest on level 2; a leading `# <title>` that repeats it
is dropped). Output is `.handoff/exports/bundle[-<title>].pdf`. The response is
`{format, bundle: true, bundle_title, exported, failed, pdf_path, typ_path,
pdf_paths: [pdf_path], documents, warnings}`; bundle warnings come from one
shared run and carry no `doc_id`, and an unreadable document is listed with its
`error` while the rest are still bundled. A template whose `conf` predates the
bundle cover (no `documents` parameter) still exports, with a
`template_outdated` warning and no document list on the cover; delete the
generated template from `.handoff/templates/` to get the current one.
The cover also shows the document id automatically; `cover_*` values the caller
omits are left off. When a customised template's `conf` lacks `doc-id`,
`version`, `status` or `organization`, the field is dropped and a
`template_outdated` warning names it.

Task documents (`task_id`, REQ-PDF-027): the documents a task is linked to are
collected and merged like a bundle (`bundle` defaults to `true` here, `false`
gives one PDF each; `bundle_title` defaults to `Task <id>: <title>`; the other
bundle options apply). A document is linked when it lists the task in its own
`task_ids`, when the task has a `doc` link to it (id or slug), when the task has
a `requirement` link whose target is the document or whose stable id names one
of its items, or when one of its items lists the task. Oldest document first.
`scope`'s other filters narrow the result (AND); `scope.task_id` has the same
reach. An unknown task, or one without linked documents, is an error.

Requirement meta blocks (`enrich`, default `true`): a heading that starts with
the stable id of an item (`## REQ-001 Title`) gets a visual block directly under
it instead of losing its attribute lines: priority and dev_stage badges,
horizontal/vertical coverage bars (the trace graph's coverage: whether
verification items / refining children exist, not whether they passed), the
titled `refines` / `verifies` targets, the implementing tasks (id, title,
status) and the acceptance criteria, each marked `✓` (a `[x]` mark in the bullet,
or a passing verification item covers it) or `○`. The `- acceptance_criteria:`
attribute and the first `受入基準:` paragraph of such an item move into the
block. The data is read-only trace data (same as `handoff_doc_req_list` /
`handoff_trace_report`). The block is drawn by `req-meta()` in
`.handoff/templates/parts/req-meta.typ`, generated on the first enriched export
and never overwritten, so it can be restyled and works with every page
template. Documents without items (no `layer`) and items the trace graph does
not know export as before; if the trace data cannot be loaded the export is
plain and carries an `enrich_unavailable` warning.

`handoff_report` accepts the same `output_dir` / `template` / `include_toc` /
`cover_*` with `format: "pdf"` (`action=generate|get`) and adds `pdf: {pdf_path,
typ_path, warnings}` to its response.

## `handoff_doc_tree`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Root (id or slug) |
| `depth` | no | Descendant levels (default `2`); the immediate parent is always included |
| `include_related` | no | Also attach `related` links (default `false`) |

## `handoff_doc_trace`

| Param | Required | Description |
|---|---|---|
| `doc_id` | yes | Start (id or slug) |
| `direction` | no | `up` (to the root) \| `down` (descendants, DFS) \| `both` (default) |

`related` documents met along the chain are appended as detours; forks going
down are listed in `branches[]`. Cycle-safe.

## `handoff_doc_graph`

| Param | Required | Description |
|---|---|---|
| `include_implicit` | no | Default `true`: add `shared_task` / `shared_scope` edges between documents sharing task ids / scope paths |
| `include_verification` | no | Add `verification_progress {total, verified}` per node |

Always covers every document (there is no focus parameter). Returns nodes,
edges (`parent_child`, each `related` kind, implicit edges) and documents
grouped by `doc_type`.

## `handoff_doc_query`

| Param | Required | Description |
|---|---|---|
| `text` | no | Prompt/query text (BM25) |
| `file_paths` | no | Files being worked on — boosts documents whose `scope_paths` match |
| `task_id` | no | Boosts documents linked to this task |
| `session_id` | no | Per-session dedup: a section already injected this session is skipped unless its content changed |
| `limit` | no | Max sections (default `5`) |
| `mark_injected` | no | Record injection for dedup (default `true`) |
| `suppress_doc_ids` | no | Exclude these documents from this call |
| `suppress_until_changed` | no | With `suppress_doc_ids` + `session_id`: keep them excluded in later calls until their content changes |

Results are staged `full` or `outline` (see SKILL.md "Staged injection").

## `handoff_doc_analyze` / `handoff_doc_import`

`handoff_doc_analyze(path, recursive?: true, flatten?: false)` — read-only scan
of a file or directory: suggested `doc_type`, tags, `scope_paths`, slug per
file, link classification (internal / external / broken), and a proposed
parent/child tree from the directory structure (`flatten: true` skips it).
Returns `{files_scanned, auto_resolved, needs_review, proposed_tree}`.

`handoff_doc_import(analyzed, overrides?, task_ids?)` — writes the reviewed
payload atomically. Each source file becomes **one** document (never split a
file yourself). `overrides[]` entries are keyed by `file` and may set `slug`,
`title`, `doc_type`, `tags`, `scope_paths`. Each `auto_resolved` entry must
carry its file's full `body` — import writes from the payload, it does not
re-read the disk.

## `handoff_doc_repair_task_ids`

No arguments besides `project_dir`. Forces a full rebuild of every requirement
SubItem's `task_ids` from the tasks' `task_links` (the source of truth), and
appends missing document-level `task_ids` from `link_type: "doc"` links
(append-only — a stale id is reported by `trace_lint`'s `task_ids_drift`
instead of removed). Always runs. Returns `{ran, sub_items_changed,
docs_changed, doc_task_ids_appended}`. Normal link changes keep these in sync
already — use this only after manual edits or imports.

## `handoff_doc_repair_frontmatter`

`handoff_doc_repair_frontmatter(slug?, dry_run?: true)` fixes the known
non-standard frontmatter shapes (a flow value such as `[]` on the line after
its key; tab indentation). `dry_run: false` rewrites the file, after which the
document reappears in `doc_list`. Unrecognized problems are listed in
`unrepaired` with their original error. A leading BOM needs no repair.
