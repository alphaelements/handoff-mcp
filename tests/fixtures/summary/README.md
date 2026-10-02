# Requirements-summary aggregation fixtures (NFR-005 minimal parity check)

These two files are a **contract shared between handoff-mcp (Rust) and
handoff-vscode (TypeScript)**: MCP's `aggregate_requirements` (Rust,
`src/mcp/handlers/docs.rs`) and the VSCode extension's `summarizeRequirements`
(TS, handoff-vscode) must derive the exact same aggregate from the exact same
input — this is FR-905 / NFR-005 (wiki/220-vmodel-integration-design.md §4.3:
"共通 JSON フィクスチャで Rust `aggregate_requirements` と TS
`summarizeRequirements` の一致をテストする"). Both sides read these files
directly rather than each hand-writing their own test fixtures, so the two
implementations cannot silently drift apart.

## Files

- `input.json` — the aggregation input: a minimal projection of what each
  document's frontmatter `verification:` block plus its `id`/`slug` look
  like. Shape: `{ "docs": [ { "id", "slug", "verification": { "items": [
  { "fragment_seq", "heading", "category", "sub_items": [ SubItem, ... ] },
  ... ] } }, ... ] }`. Every `VerificationItem`/`SubItem` field name and type
  matches the Rust structs in `src/storage/docs/model.rs` byte-for-byte
  (same JSON field names `aggregate_requirements` itself reads: `stable_id`,
  `priority`, `dev_stage`, `impl_refs`, `test_refs`, `task_ids`, `status`,
  `index`, `description`, `depends_on`). Only the fields `aggregate_requirements`
  actually reads are populated in full detail; document-level fields the
  aggregation ignores (title, tags, scope_paths, sections, ...) are omitted —
  this is intentionally *not* a full `_doc.<slug>.json`.
- `expected_output.json` — the aggregate `aggregate_requirements` must
  produce for `input.json`, in the **exact same schema as
  `.handoff/docs/_requirements_summary.json`** (the `RequirementsSummary`
  struct: `total`, `by_status`, `by_priority`, `by_category`, `coverage`,
  `task_coverage`, `items`) — current fields only, no `inputs` fingerprint
  block (that belongs to the derived-file freshness mechanism added by a
  later task, wiki/220 §4.3, and is out of scope here).

## Boundary cases covered by `input.json`

- **Multiple documents** (`doc-alpha`, `doc-beta`) — aggregation must sum
  across documents, and `task_coverage["task-1"]` must merge the SubItems
  that reference it from *both* documents.
  Two documents multiple `VerificationItem`s (`fragment_seq: 1` and `2`)
  within one document.
- **`dev_stage`/`priority` unset** (`doc-alpha` sub_item `Req A2`,
  `stable_id: "C01-1.2"`) — must fall back to `"not_started"` /
  `"unset"` respectively, and count in `by_category["C01"]` (has a
  `stable_id`) but not toward `implemented`/`tested`/`verified`.
- **`stable_id: null`** (`doc-alpha` `Section B` sub_item, `"Req B1 (no
  stable_id yet)"`) — must still count toward `total`/`by_status`/
  `by_priority`/`coverage`/`task_coverage`, serialize with `stable_id: ""`
  in `items` (empty string, `Option::unwrap_or_default()` on the Rust
  side), and must be **excluded** from `by_category` (no prefix to bucket
  it under).
- **`status: "skipped"`** (same sub_item as above) — verification-review
  status (`SubItem.status`) is independent of `dev_stage`; it only affects
  the passthrough `verification_status` field on the flattened `items`
  entry, never the `by_status`/`coverage` aggregation (which is keyed on
  `dev_stage`, not `status`).
- **`dev_stage: "verified"` implies tested+implemented** (`doc-beta`
  `Req C1`) — exercises the `is_impl`/`is_tested`/`is_verified` cascade and
  a `by_category` bucket (`C07`) with 100% coverage.

## How the Rust side reads this fixture

`src/mcp/handlers/docs.rs`, `mod requirements_summary_tests`, test
`aggregate_requirements_matches_shared_fixture`: reads both files via
`env!("CARGO_MANIFEST_DIR")` + `tests/fixtures/summary/{input,expected_output}.json`,
deserializes `input.json`'s `docs[]` into `DocMetadata` (via `DocMetadata::new`
plus the parsed `verification` field — every other `DocMetadata` field uses an
arbitrary placeholder since `aggregate_requirements` never reads them), runs
`aggregate_requirements`, serializes the result, parses both the actual and
expected JSON into `serde_json::Value`, and asserts they are equal. Comparing
parsed `Value`s (not raw JSON text) is deliberate: `RequirementsSummary`'s
`by_status`/`by_priority`/`by_category`/`task_coverage` fields are
`HashMap`s, whose serialized key order is not guaranteed — a text/byte
comparison would be flaky.

## How the TypeScript side should read this fixture (for handoff-vscode t122)

Absolute path on this machine (for the referral): 
`/home/aeuser/pro/handoff-mcp/tests/fixtures/summary/input.json` and
`/home/aeuser/pro/handoff-mcp/tests/fixtures/summary/expected_output.json`.
handoff-vscode should copy both files into its own repo (e.g.
`test/fixtures/summary/`, mirroring the `tests/fixtures/trace/` convention
already agreed for the V-model trace report in wiki/220 §3.4) and record the
handoff-mcp commit that produced this copy in its own README, the same way
wiki/220 §3.4 already specifies for `tests/fixtures/trace/`. Feed
`input.json`'s `docs[]` array into `summarizeRequirements` (or whatever the
TS aggregation entry point is named) and assert the result matches
`expected_output.json` after `JSON.parse` on both sides (object-key order
must not matter, same reasoning as the Rust test above).

## `inputs` fingerprint (t370.4, not part of this fixture)

`.handoff/docs/_requirements_summary.json` on disk carries one field this
fixture does not: `inputs`, the P-M4 write-discipline freshness fingerprint
(wiki/240-performance-design.md §4, wiki/220 §4.3 r3). It sits alongside the
`RequirementsSummary` fields above (`#[serde(flatten)]` on the Rust side,
see `PersistedRequirementsSummary` in `src/mcp/handlers/docs.rs`), never
replaces or renames them, and is recomputed via `stat` only (no file
content read) right before the file would be written:

```json
"inputs": {
  "docs_max_mtime_ns": 1732600000123456789,
  "docs_count": 4,
  "tasks_max_mtime_ns": 1732600000000000000,
  "tasks_count": 12,
  "runs_count": 0,
  "runs_max_id": null
}
```

- `docs_max_mtime_ns` / `docs_count`: max mtime (integer nanoseconds since
  the Unix epoch) and file count across every `_doc.*.md` in `docs/`
  (derived files, which never match that name pattern, are excluded).
- `tasks_max_mtime_ns` / `tasks_count`: same, across every
  `_task.<status>.json` anywhere under `tasks/` (recursive — child tasks
  live in nested directories).
- `runs_count` / `runs_max_id`: file count and lexicographically-largest
  file name under `runs/` (month subdirectories included), excluding
  `_latest.json` **and any dot-prefixed name** (t360.43 N6: a `.`-prefixed
  entry — file or directory — is always an in-flight temp write, never a
  real run to count). `crate::storage::atomic_write`/`runs::write_run_record`
  both stage a write under a `.`-prefixed name
  (`.{file_name}.tmp.{pid}.{seq}`) in the same directory before the final
  rename/hard-link, so a `readdir` landing mid-write can otherwise observe
  that transient file — this would double-count an in-flight run for the
  duration of the race and, when it is the very first run ever recorded (so
  there is nothing else to compare against), could even make `runs_max_id`
  briefly report the temp name itself. A VSCode-side reimplementation of
  this fingerprint must apply the same dot-prefix exclusion, not just the
  `_latest.json` one, to compute an identical `inputs.runs_*` value.
  **`runs/` does not exist yet** — it is created by M1 (t360.8) — so until
  then a missing directory always reports `runs_count: 0`,
  `runs_max_id: null`, not an error.

The file itself is only rewritten when this fingerprint (or the aggregate)
actually differs from what is already on disk (`_requirements_summary.json`
is unformatted/compact JSON, not pretty-printed, for the same reason). A
reader recomputes the same fingerprint from the current filesystem state
and compares — equal means "still fresh", different means "stale,
recompute". `handoff-vscode` (t122/handoff-vscode wiki/100 §3.3) is expected
to do the same comparison before treating this file as authoritative.

## `category == "check"` exclusion (M1 t360.6, wiki/220 §2.3)

`doc-beta`'s `Section D` / `ST-001 Lockout works` sub_item has
`"category": "check"` and `"layer": "system_test"` — a right-side V-model
layer item (a verification item, not a requirement). It is:

- **excluded** from `total`, `by_status`, `by_priority`, `by_category`
  (would otherwise bucket under an `"ST"` prefix), `coverage`, and
  `task_coverage` (its `task_ids: ["task-3"]` must **not** create a
  `task_coverage["task-3"]` entry at all) — the same treatment a "not a
  requirement" item gets everywhere else in this aggregate;
- **included** in `items[]`, carrying its `category` (`"check"`) and
  `layer` (`"system_test"`) fields so a caller can still render it (e.g. a
  V-model trace view) without re-reading document frontmatter.

Every `items[]` entry (not just the new one) now carries `category`
(`SubItem.category`, verbatim — `"requirement"` for every pre-existing
entry here) and, when set, `layer` (`sub.layer.or(doc.layer)` — absent/
`None` for every pre-existing entry, since none of them set a layer). Both
are genuinely new fields on `SummaryRequirementItem`, so this is the one
place this fixture's *existing* entries gained keys rather than only having
new entries appended — the existing boundary-case *values* (stable_id,
priority, dev_stage, ...) are unchanged. The TS side (handoff-vscode t131)
must mirror the same two additions: category-based exclusion from every
count, plus `category`/`layer` passthrough on every summarized item.

## No `state` field (t360.13, wiki/220 §2.7/S4)

`SummaryRequirementItem`/`expected_output.json` items never carry a `state`
key. An earlier revision (M1 t360.10) briefly added one, populated only by an
`aggregate_requirements_with_states` variant with no production caller — the
two real call sites (`write_requirements_summary`, `handoff_doc_req_status`)
only have `docs: &[DocMetadata]`, never the runs/task-link context a
`crate::trace::TraceGraph` needs to compute `state`, so the field would
always serialize as absent here. t360.13 removed both the field and the
dead `_with_states` variant rather than leave a permanently-empty contract
key. Verification `state` is available from `_trace_report.json`'s
`items[]` instead (`tests/fixtures/trace/`) — built from a real
`TraceGraph`. `layer`/`category` (both described above) are unaffected and
remain part of this fixture.

## Extending this fixture further

Keep further additions additive — append new `VerificationItem`/`SubItem`
entries rather than editing the existing ones, so this fixture keeps testing
the exact boundary cases documented above without churn.
