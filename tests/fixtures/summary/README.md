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

## Extending this fixture later

t360.6 will add `category: "check"` items to `input.json` (and the
corresponding exclusion to `expected_output.json`) once the M1 exclusion
rule for `category == "check"` items lands on both sides (MCP side t360.6,
VSCode side handoff-vscode t131). Keep new sub_items additive — append new
`VerificationItem`/`SubItem` entries rather than editing the existing ones,
so this fixture keeps testing the exact boundary cases documented above
without churn.
