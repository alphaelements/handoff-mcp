# V-model trace report fixtures (NFR-005 contract check)

This directory is a **contract shared between handoff-mcp (Rust) and
handoff-vscode (TypeScript)** for `.handoff/docs/_trace_report.json`
(wiki/220-vmodel-integration-design.md §3.4): MCP builds this file from a
real `crate::trace::TraceGraph`, and handoff-vscode's V-model view must read
the exact same shape without re-implementing the derivation engine in
TypeScript (NFR-005). Both sides should read these files directly rather
than hand-writing their own fixtures, so the two implementations cannot
silently drift apart — mirrors `tests/fixtures/summary/`'s existing
convention for `_requirements_summary.json`.

## Files

- `project/handoff/` — a complete, ready-to-run `.handoff/` project tree
  (four layer documents — `requirement`/`basic_spec`/`acceptance`/
  `system_test` — one task with a requirement link, and one recorded
  `runs/*.json` execution result). Named `handoff/` **without** a leading
  dot, deliberately: the repo's top-level `.gitignore` has an unanchored
  `.handoff/` rule (project-local handoff data is never committed), which
  would otherwise swallow a literally-named `.handoff/` directory anywhere
  in the tree, including under `tests/fixtures/`. A consumer must copy this
  directory to `<project>/.handoff/` before pointing a real
  `handoff-mcp`/handoff-vscode instance at it (see
  `tests/trace_report_contract_fixture_e2e.rs`'s `setup_project` for the
  Rust side's copy step).
  - `runs/_latest.json` is intentionally **absent** from this fixture (the
    nested `project/handoff/.gitignore` excludes it, matching a real
    project's own convention) — it is a derived cache `runs::sync` rebuilds
    automatically from `runs/*.json` on first use, so there is nothing to
    commit.
  - `docs/_trace_report.json` is intentionally **absent** — that is exactly
    the file under test; a consumer generates it by calling
    `handoff-mcp trace report --project-dir <project>` (or the
    `handoff_trace_report` MCP tool) against the copied fixture.
  - `docs/_requirements_summary.json` **is** present (a real project would
    have one too, refreshed by the same `handoff_doc_save` calls that built
    this fixture) — harmless to `trace_report`, included for realism.
- `expected_output.json` — the exact `_trace_report.json` content
  `handoff-mcp trace report` must produce for `project/handoff/`, pretty-
  printed for readability (the real derived file itself is compact/
  unformatted — see "Formatting" below). Generated once from this project by
  running the real binary and copying its output verbatim (the same
  reference-implementation-as-fixture relationship
  `tests/fixtures/summary/` already documents for `expected_output.json`
  there); re-verified as a **regression** fixture by
  `tests/trace_report_contract_fixture_e2e.rs`, not derived from independent
  reasoning about the algorithm.

## What the fixture project covers

- **`requirement`** doc: `REQ-001` (priority P0, implemented by task
  `t-fixture-1`, verified by `AT-001`) and `REQ-002` (priority P1, no
  verifier and no refining child — an `unverified` **and** `unrefined` gap).
- **`basic_spec`** doc: `SPEC-001` refines `REQ-001` (no refining child of
  its own — an `unrefined` gap; no task implements it either).
- **`acceptance`** doc: `AT-001` verifies `REQ-001` (`method: manual` —
  right-side check item, `category: "check"`, no run recorded yet — state
  `not_run`).
- **`system_test`** doc: `ST-001` verifies `SPEC-001` (`method: auto`), with
  one recorded `runs/*.json` result (`pass`) — state `passing`, and
  `items[].last_run` populated for this one item only.
- Task `t-fixture-1` implements `REQ-001` (`requirement_ids: ["REQ-001"]`) —
  covers the `tasks: [{id, role}]` field on an `items[]` entry.

This exercises every top-level key of the persisted shape: multiple in-use
layers (`trace_layers.in_use`, `source: "auto"`), a full `coverage` block per
layer (`horizontal`/`vertical`/`state`), a non-empty `gaps[]` with two
distinct kinds (`unverified`, `unrefined`) plus their `gap_counts`, and
`items[]` covering both sides (`left`/`right`), both categories
(`requirement`/`check`), a task link, a `last_run`, and an item with neither.

## `schema_version` and `inputs`

`expected_output.json` carries `schema_version: 1` (bump this fixture's
expectation, and `crate::mcp::handlers::trace::TRACE_REPORT_SCHEMA_VERSION`,
together whenever the persisted shape changes in a way a reader must react
to) and an `inputs` fingerprint (wiki/220 §4.3 r3, the same
`docs_max_mtime_ns`/`docs_count`/`tasks_max_mtime_ns`/`tasks_count`/
`runs_count`/`runs_max_id` shape `_requirements_summary.json` carries — see
`tests/fixtures/summary/README.md`'s "`inputs` fingerprint" section for the
field-by-field definition, reused verbatim via
`crate::mcp::handlers::docs::compute_derived_inputs`).

`docs_max_mtime_ns`/`tasks_max_mtime_ns` are **excluded from comparison** —
copying this fixture into a fresh tempdir necessarily changes on-disk file
mtimes, so these two fields legitimately differ from `expected_output.json`
on every test run (`tests/trace_report_contract_fixture_e2e.rs` zeroes both
sides' values for these two keys before comparing). Every other field
(`docs_count`, `tasks_count`, `runs_count`, `runs_max_id`, `schema_version`)
is a structural property of the fixture's fixed file set — copying does not
change it — and is compared exactly.

`runs_count`/`runs_max_id` exclude any dot-prefixed name under `runs/` in
addition to `_latest.json` (t360.43 N6) — see
`tests/fixtures/summary/README.md`'s `inputs` section for why (an
`atomic_write`/`write_run_record` in-flight temp file is always
dot-prefixed, and a `readdir` landing mid-write would otherwise fold it into
this count). A VSCode-side reimplementation of this fingerprint must apply
the same exclusion.

## M2-03: `coverage.<layer>.horizontal`/`.vertical` gained `partial`/`waived` (v1 -> v2 shape)

wiki/260-vmodel-m2-design.md §3.1/§5.1 r2: M2 adds two new keys to every
`coverage.<layer>.horizontal`/`.vertical` object — the shape is now
`{covered, partial, uncovered, waived, na}` where M1 only had `{covered,
uncovered, na}`. This is a **semantic** v1 -> v2 change even though it is
byte-shape-additive (existing keys keep their old meaning for items that
have no acceptance criteria / no waiver, and no key is removed or renamed):
v1's `covered` count folded in what v2 now splits out separately as
`partial` (deep coverage / partially-verified acceptance criteria), and v1's
`uncovered` folded in what v2 now splits out as `waived`. A reader computing
a coverage percentage must use v2's `covered / (total - na - waived)`
(wiki/260 §3.1) rather than v1's `covered / (total - na)`, or it will
silently over/under-count once any item in the project uses `partial`/
`waived` classification. `expected_output.json` demonstrates this directly:
`coverage.requirement.vertical` is `{covered: 0, partial: 1, uncovered: 1,
waived: 0, na: 0}` — `REQ-001` reclassified from v1's `covered` to v2's
`partial` because its refining child `SPEC-001` is itself `uncovered`
("deep coverage", wiki/260 §3.1/§11 Q7).

`schema_version` stays `1` for this change (not bumped to `2`) — the
decision recorded here deliberately departs from this file's own general
rule above ("bump ... together whenever the persisted shape changes in a
way a reader must react to"): bumping `TRACE_REPORT_SCHEMA_VERSION` is
`src/mcp/handlers/trace.rs`, M2-04's (developer B's) file in this session's
scope split, so M2-03 (developer A) intentionally left it untouched rather
than encroach; the reader-must-react fact above (recompute the percentage
formula) is instead captured explicitly in prose here and in wiki/260 §3.1,
which any consumer diffing this fixture's `expected_output.json` against an
older copy will also notice directly. A future session bumping
`TRACE_REPORT_SCHEMA_VERSION` to `2` for this or a related M2 change should
update this fixture's `schema_version` value to match in the same change.

## M2-03 (round 3 rework): `items[].profile` added

wiki/260-vmodel-m2-design.md §2.1 規則 4: every `items[]` entry now also
carries `profile` — `TraceGraph::item_profile`'s sorted, deduped effective
profile name(s) reached by that item's own tree (empty when no named
profile applies, e.g. project-default "auto" with no `[trace] profile`/
`trace_profile` override anywhere in this fixture, which is why all five
items here show `"profile": []`). This is purely additive to every `items[]`
object; no other key's shape or value changed (`schema_version` stays `1`).
Round 2's integration review found `TraceGraph::item_profile` had been added
but never wired into any JSON output (`build_report_items` /
`handle_trace_slice`'s item builder) — this fixture's `expected_output.json`
was regenerated from the real binary once that wiring landed.

## Writing trigger (manager decision, M-S11/t360.13)

`_trace_report.json` is **not** rewritten on every `handoff_update_task` /
`handoff_doc_verify` / `handoff_doc_update_section` call. wiki/220 §2.4 step
7 (revised 2026-09-27) is explicit that this file is *not* written there —
only `_requirements_summary.json` is ("`_trace_report.json` はここでは書か
ない。§3.4 の書き込み契機を参照") — an earlier revision of that step's
wording had instead nominally asked for the same trigger
`_requirements_summary.json` uses ("summary と同じ契機で書く"), which is
what motivated this section in the first place; that wording has since been
retracted in favor of the explicit exclusion. Building a full
`crate::trace::TraceGraph` measured ~107-180ms at JA/L scale (t360.10's perf
bench) — far beyond PR-1's ≤50ms `handoff_update_task` budget — so wiring it
into those hot paths would regress PR-1/PR-3/PR-4. It is **only** (re)written
from `handoff_trace_report` (and CLI `trace report`, which dispatches to the
same handler) — already pays the graph-build cost for its own response
regardless. A `trace report` call with a non-empty `layers` override does
**not** write the file (the override shapes only that call's response;
`inputs` cannot record it).

`handoff_trace_record` (and CLI `trace record`) deliberately does **not**
also rebuild/write `_trace_report.json`, even though an earlier revision of
this task tried exactly that: measured p50 with the rebuild wired into
`handoff_trace_record` was ~271ms at L scale, against that op's own ~100ms
PR-4 budget (`tests/perf_budgets.toml`'s `trace_record` entry) — a ~2.7x
regression, reverted once measured
(`tests/trace_record_e2e.rs::trace_record_never_writes_the_trace_report_derived_file`
guards this). Recording a run therefore leaves `_trace_report.json` stale
(by its `inputs` fingerprint) until the next `trace report` call.
`handoff_trace_record` *does* (t360.43 S3, wiki/220 §3.1: "記録後に
`_latest.json` と summary を更新する") refresh
`_requirements_summary.json` itself — a much cheaper P-M4 stat-and-compare
write than this file's full `TraceGraph` build, so it does not reproduce the
same regression.

Freshness for any other reader (handoff-vscode) is still guaranteed by the
`inputs` fingerprint above: a stale `_trace_report.json` is detectable by
recomputing the same fingerprint from the current filesystem state, and
handoff-vscode's design (wiki/100 §3.3) reacts to a mismatch by calling
`handoff-mcp trace report` itself — including right after a `trace record`
call, which is exactly how a fresh `state` reaches `_trace_report.json` in
practice.

## Formatting

The real `.handoff/docs/_trace_report.json` is **compact JSON, not
formatted** (`serde_json::to_string`, not `to_string_pretty`) and carries
**no generated timestamp** (one would change on every write for no reason,
defeating the "only write when content differs" discipline below) — same
posture as `_requirements_summary.json`. `expected_output.json` in this
directory is pretty-printed purely for human/diff readability; a consumer
comparing against it should parse both sides to a value and compare
structurally (`JSON.parse` on the TS side, `serde_json::Value` equality on
the Rust side — object key order must not matter), never a byte/text
comparison.

The file is only actually rewritten when its content — including the
`inputs` fingerprint — differs from what's already on disk (P-M4, wiki/240
§4): saving a document or task unrelated to the trace graph's own content
still moves `inputs` (a different `docs_count`/`docs_max_mtime_ns`), so the
next `handoff_trace_report` call rewrites the file purely to record the new
fingerprint even though `trace_layers`/`coverage`/`gaps`/`items` are
unchanged; a request that changes nothing at all (identical content **and**
identical `inputs`) writes nothing.
`tests/trace_report_derived_file_e2e.rs` (not this fixture — a separate,
synthetic-project test) covers both cases directly.

## `state` is not read from `_requirements_summary.json` (t360.13)

`_requirements_summary.json`'s `items[]` (`tests/fixtures/summary/`) never
carries a `state` field — an earlier revision briefly added one with no
production caller, since aggregating `_requirements_summary.json` never has
access to runs/task-link context. Verification `state` is only available
here, in `_trace_report.json`'s `items[]`, built from a real `TraceGraph`.
A VSCode reader wanting `state` must read `_trace_report.json`, not treat a
missing `state` key on a `_requirements_summary.json` item as "unknown
state" that it should try to backfill from that file.

## Extending this fixture further

Keep further additions additive — add a new document/item/task/run rather
than editing the existing ones, and regenerate `expected_output.json` by
running the real binary again (do not hand-edit it), so this fixture keeps
testing the exact scenarios documented above without churn.
