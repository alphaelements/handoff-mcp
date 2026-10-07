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

## `v1/` and `v2/` (M2-07)

This directory holds two independent fixture projects, each with its own
`project/handoff/` and `expected_output.json` (everything below this section
describes their shared shape/conventions — read it once, it applies to
both): `v1/` is the original, minimal fixture project (unchanged in content,
simply relocated here) and `v2/` is a new, comprehensive project added for
wiki/260-vmodel-m2-design.md §5.1's schema v2 (see `v2/README.md` for what
it covers). `tests/trace_report_contract_fixture_e2e.rs` runs both.

`v2/expected_output.json` carries `schema_version: 2`, generated against
`v2/project/handoff/`. `v1/` carries **two** expected-output files against
the one unchanged `v1/project/handoff/` project (round-2 rework — see "M2-07:
`schema_version` 2" below for why): `v1/expected_output.json` is the frozen,
real **`schema_version: 1`** sample (the exact M1-era binary output, restored
from git history `HEAD` at the M2-07 rework commit) that the live E2E tests
do **not** drive the binary against any more — its only consumer is the
additive-compatibility test below and any dual-reader (handoff-vscode `v1`
path) that needs a genuine pre-M2-07 sample to test against.
`v1/expected_output_v2.json` is today's binary's actual output for that same
project (`schema_version: 2`) — this is what
`tests/trace_report_contract_fixture_e2e.rs`'s `v1` CLI/MCP tests compare
the live binary's output to.

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

## M2-05: `coverage[layer].suspect` added

wiki/260-vmodel-m2-design.md §3.2's closing bullet: every `coverage[layer]`
entry now also carries `suspect: {links, tasks, results, items}` — per-layer
tallies of the 3 suspect kinds `handoff_trace_suspect` derives (M2-05),
folded into the same `TraceGraph::build` this fixture already exercises.
This fixture's project has no `refines`/`verifies` reference whose upstream
changed after being baselined (every link here was created in one shot, all
baselines match their upstream's current hash), so every `suspect` object in
`expected_output.json` is `{links: 0, tasks: 0, results: 0, items: 0}` —
purely additive, no other key's shape or value changed (`schema_version`
stays `1`, same reasoning M2-03's entry above already gives for not bumping
it from this session's scope split). Regenerated from the real binary
(`handoff-mcp trace report --project-dir <copy of project/handoff>`) with
only this new key's zeros added — every other value byte-identical to the
pre-M2-05 fixture (confirmed via a diff limited to added/removed keys, not a
blind full regeneration, to keep this a purely additive contract change).

## M2-07: `schema_version` 2 (`v1/` keeps a frozen `schema_version: 1` sample, `v2/` added)

wiki/260-vmodel-m2-design.md §5.1/§11 Q2: `TRACE_REPORT_SCHEMA_VERSION` is
now `2` — unlike every prior additive change recorded in this file (M2-03,
M2-05 above), this one *is* a version bump, because v2 changes the meaning
of existing keys (`coverage.<layer>.horizontal`/`.vertical`'s `covered`
splits into `covered`+`partial`, `uncovered` splits into `uncovered`+
`waived` — already true since M2-03, just now reflected in the version
number itself) and adds several new top-level/`items[]` fields a reader must
know to look for: `layer_defs`, `profile`, `suspect_counts`, `tasks[]`, and
per-item `def_hash`/`coverage`/`suspect`/`reverify`/`approval`/`acceptance`/
`implicit_of`/`derived`/`waivers`/`from`, plus `last_run.stale` and
`inputs.config_fnv` (see wiki/260 §5.1/§5.2 for the full field-by-field
definition). `next_actions` is **not** part of this change (M2-10).

**Round-1 rework correction**: an earlier revision of this change
regenerated `v1/expected_output.json` from the current binary, which made it
carry `schema_version: 2` — deleting the repo's only committed
`schema_version: 1` sample even though handoff-vscode's planned dual reader
(§5.1 reader rule "1 か 2 なら読む"; §5.5/§11 Q2, M2-20's completion
condition: a v1+v2 reader must ship before MCP releases v2) needs a genuine
v1 file to test its v1 code path against. This was reverted: `git show
HEAD:tests/fixtures/trace/expected_output.json` (the pre-M2-07 commit, where
this fixture still lived directly under `tests/fixtures/trace/`) was
restored verbatim as `v1/expected_output.json`, and the regenerated
`schema_version: 2` content was kept instead as `v1/expected_output_v2.json`
— the file `tests/trace_report_contract_fixture_e2e.rs`'s live `v1` CLI/MCP
tests actually compare the running binary against now. A new test,
`v1_frozen_fixture_is_schema_version_1_and_additively_preserved_in_v2` in
that same file, mechanically guards the two files' relationship: the frozen
file's `schema_version` is `1`, and every key/value it contains (aside from
`schema_version` and the two always-differing mtime fields) is present
unchanged in `expected_output_v2.json` — i.e. the M1→M2 change really was
additive for this project, not just by inspection but by an assertion that
fails the moment a future change breaks that promise.

`v2/project/handoff/` is a new fixture exercising every v2-specific
scenario the schema needs regression coverage for — see `v2/README.md`,
which also documents the one spot it departs from "regenerate from the real
binary, never hand-edit" (an `unbaselined` link, simulating pre-M2 legacy
data that cannot be produced through any live MCP call sequence since M2
baselines every new link immediately on sync).

## M2-10: `next_actions` (wiki/260 §5.1/§4.5/§3.5)

Adds the `next_actions` top-level array M2-07's entry above explicitly
deferred — the top 20 (`PERSISTED_NEXT_ACTIONS_LIMIT`, project-wide, every
kind) ranked next actions, same `{rank, kind, item?, task?, priority?,
reason, suggest: {tool, arguments}}` shape `handoff_trace_next`'s own
`actions[]` returns (`src/trace/next.rs`'s pure `derive_next_actions`, called
once more from `build_persisted_trace_report_body` against the same graph/
`trace_input` that request already built — no second `TraceGraph::build`).
`schema_version` stays `2` (purely additive — a new top-level key, no
existing key's shape or meaning changed, same reasoning M2-05's
`coverage.<layer>.suspect` entry above gives for not bumping again). Both
`v1/expected_output_v2.json` (9 actions for that project's smaller scenario)
and `v2/expected_output.json` (15 actions, covering `review_suspect`/
`write_verification`/`refine`/`create_task` from that fixture's existing
suspect/partial/waived/derived scenarios — no new document/item was added
for this change, `next_actions` is wholly derived from state the fixture
already exercises) were regenerated from the real binary with only this new
key added — confirmed via a diff limited to the added `next_actions` key (and
the two always-differing mtime fields), not a blind full regeneration, same
discipline the M2-05/M2-07 entries above describe.

## M3: `approval_draft` task blocker and `relink_candidate` next action

wiki/270-vmodel-m3-design.md §4.4 (approval 3-state lifecycle, t360.40.04)
adds `approval_draft` as a possible key in `tasks[].blockers` — a task is
blocked when a linked item it verifies/implements has a draft (not yet
submitted) approval record. §4.7 (t360.40.13, FR-204) adds a new
`next_actions`/`handoff_trace_next` kind, `relink_candidate`: detected when a
`detailed_spec` (or other newly-added mid-layer) item now refines a
`basic_spec` item that a `unit_test`/task already links directly, suggesting
the link be moved down to the new intermediate layer instead.

Both `v1/expected_output_v2.json` and `v2/expected_output.json` were
regenerated from the real binary against their existing, unmodified fixture
projects — no new document/item/task was added for this change. `schema_version`
stays `2` (purely additive — an existing map key's possible value set grows,
and `next_actions` gains a new `kind`, neither changes any existing key's
shape). Confirmed via a diff limited to the added keys (and the two
always-differing mtime fields), not a blind full regeneration:

- `v1/expected_output_v2.json`: `tasks[0].blockers.approval_draft: 1` added,
  plus two new `next_actions` entries (`kind: "relink_candidate"`, rank 9) —
  one for `AT-001` (suggesting `AT-001` relink its `verifies` from `REQ-001`
  to `SPEC-001`) and one for `t-fixture-1` (suggesting the task relink its
  `requirement_ids`/`requirement_roles` the same way) — this fixture's
  project is the one that has a `basic_spec` item (`SPEC-001`) added after
  `AT-001`/`t-fixture-1` already linked `REQ-001` directly, so it is the
  scenario `relink_candidate` is meant to catch.
- `v2/expected_output.json`: `tasks[0..2].blockers.approval_draft: 1` added
  to all three tasks (every item this fixture's tasks link has a draft
  approval record); no `relink_candidate` entries, since this fixture's
  `basic_spec`/`detailed_spec` items were never linked directly by a
  task/`unit_test` the way `v1/`'s `SPEC-001`/`AT-001`/`t-fixture-1` were —
  see `v2/README.md` for what this fixture covers instead.
- `v1/expected_output.json` (the frozen `schema_version: 1` sample) is
  untouched, per the "Extending this fixture further" rule below.

## Extending this fixture further

Keep further additions additive — add a new document/item/task/run rather
than editing the existing ones, and regenerate `expected_output.json` by
running the real binary again (do not hand-edit it), so this fixture keeps
testing the exact scenarios documented above without churn. The one
exception is `v1/expected_output.json` itself (see "M2-07" above): it is a
frozen historical sample and must never be regenerated from a post-M2-07
binary — changes to the `v1/project/handoff/` project's live-binary output
belong in `v1/expected_output_v2.json` instead.

## FR-510: `layer_statuses` and `project_status`

wiki SPEC-510 adds two top-level keys: `layer_statuses` (`{<in-use layer>:
not_started|in_progress|under_review|verified|approved}`, in `trace_layers.in_use`
order) and `project_status` (`not_started|in_progress|under_review|verified|complete`
— the lowest layer status, `complete` when every layer is approved). `verified` is
derived (all items passing and no gap attributed to the layer); `under_review` /
`approved` come from explicit records in `.handoff/trace/layer_status.json` (set via
`handoff_trace_update`'s `set_layer_status` op) and are dropped when the layer stops
being verified. `schema_version` stays `2` (purely additive). Both
`v1/expected_output_v2.json` and `v2/expected_output.json` were regenerated from the
real binary with only these two keys added (confirmed by a key-level diff: nothing
removed, nothing else changed). No fixture carries a `layer_status.json`, so neither
shows `under_review`/`approved`.
