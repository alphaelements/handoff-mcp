---
name: handoff-trace
description: "V-model traceability — layers, profiles, links, suspect/reverify, and the trace_* tool family. Triggers on 'トレーサビリティ', 'V字モデル', '要件リンク', 'suspect', 'trace_report', 'trace_update', 'traceability', 'requirement coverage', 'V-model', or when a task mentions layer documents, acceptance criteria blocks, or verification matrices."
---

# Handoff Trace Skill (V-model traceability)

Everything about **layer documents** — the V-model requirement/spec/test
traceability system built on top of `handoff_doc_*`. This is the single
source of truth for the concept; `handoff-docs`/`handoff`/session-loop link
here instead of re-explaining it.

Design reference: wiki/260-vmodel-m2-design.md (M2) and
wiki/220-vmodel-integration-design.md (M1).

## When to use

- Setting `layer=` on a document, or writing items inside one
- Declaring custom layers/profiles in `config.toml`
- Recording or ingesting test results against V-model items
- Checking coverage, gaps, suspects, or "what to do next" in the trace graph
- Bulk-editing items/links/results via `trace_update`
- Wiring session-loop/research-loop to use layered requirements

## 1. Layers

A **layer document** is any document with `doc_save(layer=...)` set. Once
set, its Markdown body — not `doc_verify` calls — is the source of truth for
its items: every `doc_save`/`doc_update_section` call (and
`doc_verify(sync)`) re-parses the body and rebuilds the verification matrix
from it.

### The 6 built-in layers

| layer id | side | level | pairs with | default ID prefixes |
|---|---|---|---|---|
| `requirement` | left | 1 | `acceptance` | `REQ`, `FR`, `NFR` |
| `basic_spec` | left | 2 | `system_test` | `SPEC`, `BS` |
| `detailed_spec` | left | 3 | `unit_test` | `DS` |
| `acceptance` | right | 1 | `requirement` | `AT`, `AC` |
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

### Custom layers (wiki/260 §2.1)

Declare project-defined layers beyond the 6 built-ins with `[[trace.layer]]`
(both halves of a left/right pair must be declared, pointing back at each
other; an invalid declaration — duplicate id, non-reciprocal `pair`, same
side, `level < 1`, or a colliding `id_prefixes` entry — is disabled with a
warning rather than failing the whole project):

```toml
[[trace.layer]]
id = "ux_spec"
side = "left"
level = 2
pair = "usability_test"
id_prefixes = ["UX"]

[[trace.layer]]
id = "usability_test"
side = "right"
level = 2
pair = "ux_spec"
id_prefixes = ["UXT"]
```

## 2. Profiles (wiki/260 §2.1)

The "used layers" set is resolved in priority order:

1. An explicit `[trace] layers` in `config.toml` wins outright.
2. Otherwise, the project default profile's own `layers`.
3. Otherwise, auto-detection from which layers actually have items (M1
   behavior).

If both `[trace] layers` and a profile are set, `layers` wins and a warning
is emitted.

### Built-in profiles

| profile | layers | implicit_acceptance | display overrides |
|---|---|---|---|
| `minimal` | requirement, acceptance | true | — |
| `standard` | + basic_spec, system_test | false | — |
| `full` | all 6 | false | — |
| `bugfix` | requirement, acceptance | true | requirement → 再現条件, acceptance → 回帰テスト |

Set a project default with `[trace] profile = "standard"` (one of the
built-ins, or a key under `[trace.profiles.<name>]` extending one of them):

```toml
[trace]
profile = "standard"

[trace.profiles.web]
extends = "standard"
layers = ["requirement", "ux_spec", "acceptance", "usability_test"]
implicit_acceptance = false
```

Override per document with `doc_save(trace_profile="bugfix")` (empty string
clears the override).

### Tree-inheritance rules (wiki/260 §2.1 規則 1〜4)

A `trace_profile` override is scoped to the **whole requirement tree
reachable from that document's items**, not just the items that document
itself owns:

1. **Effective profile set** of an item = the profile(s) of every root it's
   reachable from. Walk a left-side item's `refines` upward (a
   right-side/verification item instead walks its `verifies` targets) to the
   item(s) with no further parent — each root's own document supplies the
   profile (its override, or the project default if it has none). An item
   reachable from both an overridden root and a default-profile root (e.g. a
   shared verification item) carries **both** profiles.
2. **Effective used layers** = the union of every reached profile's layers
   (厳しい側に倒す — the stricter side wins). If `[trace] layers` is
   explicit, that is the "project default" used layers (a non-overridden
   root uses it; an overridden root's subtree uses its own profile's layers
   instead, `[trace] layers` does not apply there).
3. An item whose own layer isn't in its effective used-layer set drops out
   of coverage/state entirely (same as an out-of-scope layer in M1) — lint
   rule `layer_outside_profile` (info) reports it.
4. Display names belong to the profile. The project default's names are in
   `_trace_report.json`'s `layer_defs[].display_name`; an override's names
   are under `profile.overrides[].display_names`; `items[].profile` lists
   each item's effective profile name(s), sorted.

The implicit acceptance-verification materialization (§3 below) uses the
*document's own* `trace_profile` (or the project default) at sync time — it
does not walk the tree, because layer sync doesn't build a graph.

## 3. Body notation (wiki/260 §2.2)

A heading whose text **starts with an allowed ID**
(`<PREFIX>-<digits>[letter]`, e.g. `SPEC-012`, `AT-001a`, `ST-LOGIN-01`;
`:`/`.` right after the ID is a separator, not part of the title) is one
item, regardless of heading level. A heading that doesn't start with a
recognized-but-unlisted prefix is just an ordinary section heading; an
ID-*looking* heading with a disallowed prefix (e.g. `HTTP-2`) is silently
left alone (with a warning in the response).

```markdown
### REQ-003 ログイン失敗時のアカウントロック

- priority: P1
- rationale: 総当たり攻撃の抑止（2026-08 監査の指摘）

5回連続で認証に失敗したアカウントを 15 分間ロックする。

受入基準:
- AC1: Given 同一アカウントで4回失敗済み When 5回目に失敗する Then アカウントがロックされる
- AC2: WHEN アカウントがロック中 THE SYSTEM SHALL 正しいパスワードでもログインを拒否する

### SPEC-020 監査ログの保存形式

- refines: REQ-003
- derived: 実装方式から必要になった項目（上位要件なし）

### ST-051 画面文言の確認

- verifies: SPEC-020
- waive-verify: 文言のみのため目視レビューで代替（2026-09 合意）
```

### Attribute line placement — heading → attributes → body (strict order)

**The attribute block is only ever recognized as the first contiguous
bullet list immediately after the heading** (blank lines before it are
fine; a blank line in the middle ends the block). A `- priority: ...` /
`- assignee: ...` / `- refines: ...` / etc. line written *anywhere else* —
after the body prose has started, after a blank line breaks the leading
bullet run, or inside a second bullet list further down — is **never**
parsed as an attribute. It is silently left as ordinary body text, and the
attribute it was trying to set is simply never applied. `trace_lint`'s
`attribute_after_body` rule (warning) catches this, but the correct fix is
always to move the line back to right after the heading — write it in the
right order from the start.

**Correct order, one copy-pasteable template per layer:**

```markdown
### REQ-001 <short requirement title>

- priority: P1
- rationale: <why this requirement exists>

<Requirement body/statement — comes AFTER the attribute block.>
```

```markdown
### SPEC-001 <short spec item title>

- refines: REQ-001
- priority: P1

<Spec body — comes AFTER the attribute block.>
```

```markdown
### DS-001 <short design item title>

- refines: SPEC-001

<Design detail body — comes AFTER the attribute block.>
```

```markdown
### AT-001 <short check title>

- verifies: REQ-001
- method: manual
- assignee: alice

<Acceptance test body — comes AFTER the attribute block.>
```

```markdown
### ST-001 <short check title>

- verifies: SPEC-001
- method: auto
- test: tests/<file>.rs::<test_fn>
- needs: system_test

<System test body — comes AFTER the attribute block.>
```

**BAD — attributes written after the body (the attribute lines below are
silently ignored, not applied):**

```markdown
### REQ-001 <short requirement title>

<Requirement body/statement written first.>

- priority: P1
- rationale: <this line is NEVER parsed as an attribute — it stays as
  plain body text, and priority/rationale are never set>
```

```markdown
### SPEC-001 <short spec item title>

Some description text right after the heading, with no leading bullets.

- refines: REQ-001
- priority: P1
```

In the second BAD example, `refines`/`priority` are lost even though the
lines *look* correct, because the very first non-blank content after the
heading is prose, not a bullet — once that happens, the whole attribute
scan for that item never runs at all.

### Attribute block (first contiguous bullet list after the heading only)

| key | meaning |
|---|---|
| `refines` / `verifies` | comma-separated upstream IDs; `verifies` may target one AC with `REQ-003#AC1` |
| `layer` | per-item layer override (lets one document mix a defining layer with its paired verification layer) |
| `priority` | `P0`-`P3` |
| `method` | `manual`\|`auto`\|`visual`\|`review` |
| `test` | `path::name`, repeatable |
| `rationale` | free-text justification |
| `derived` | reason; marks an item with no upstream link as intentional (no `orphan` gap) |
| `waive-verify` / `waive-refine` | reason; exempts that axis from the coverage requirement |
| `from` | scaffold provenance — the id this item was generated from |
| `assignee` / `needs` | reserved (FR-307/FR-202); stored verbatim, no behavior yet |

`derived`/`waive-verify`/`waive-refine` require a non-empty reason — an
empty one is dropped with a warning, never stored unexplained (nothing
should silently excuse itself from coverage). Everything else after the
heading (unknown-key lines, later bullet blocks, prose) is the item's
body/statement.

### Acceptance criteria block and implicit verification (wiki/260 §2.2/§2.5)

A `受入基準:`/`Acceptance criteria:` line followed by a bullet list, right
after the item's heading (or its attribute block), is parsed into per-item
acceptance criteria — `AC1`, `AC2`, … (an authored `AC<n>:` label, or a
position-assigned one with a warning), each classified `gwt`
(Given/When/Then), `ears` (WHEN/WHILE/WHERE/IF … SHALL), or `text` (neither).

Under a `minimal`/`bugfix`-shaped profile (project default, or the
document's own `trace_profile` override), each acceptance criterion is also
materialized as its own acceptance-verification `SubItem` (`REQ-003#AC1`,
`origin: "body"`, `implicit_of: "REQ-003"`) right after its parent — record
results and link tasks against it the same as any explicit verification
item. A `standard`/`full`-shaped profile does not materialize these; write
an explicit verification item instead.

**What "横方向カバレッジ covered" means under minimal (wiki/260 §3.1)**: a
requirement with an acceptance-criteria block is *always* horizontally
`covered` under a profile with `implicit_acceptance: true` — the implicit
verification items exist automatically, so there is nothing left uncovered
to report. Progress toward actually *passing* those checks is tracked
separately via `state` (passing/failing/not_run/…), not via coverage. Do not
read "covered" under minimal as "verified" — check `state` for that.

Each item also carries a `def_hash` (title + body-minus-acceptance-criteria
+ acceptance criteria, NFKC-normalized) — distinct from `body_hash`
(unchanged M1 key set: `refines`/`verifies`/`layer`/`priority`/`method`/
`test` stripped from the statement) — used for suspect detection (§5) once
an item has a recorded baseline.

## 4. Links and baselines

A `refines`/`verifies` reference records the upstream's `def_hash` (or
`ac_hash` for an `X#ACn` sub-reference) as a **baseline** the moment the link
is first added (`SubItem.link_baselines` for item↔item links,
`TaskLink.baseline_hash` for a task's requirement link). This is what lets
`trace_suspect`/`trace_lint` later tell you the upstream changed since the
link was made, without storing a separate "suspect" flag anywhere (E1 —
nothing propagates; each link's own baseline vs. current-hash comparison is
the whole story).

A link that predates M2 (or was authored before its upstream existed) has no
baseline at all — it is `unbaselined`, never a suspect, until
`trace_suspect(action="baseline")` fills one in (§5 below; see also §7
below for the M1→M2 migration path).

## 5. Three states, three suspect kinds

### 5.1 The three axes (wiki/260 §3.3, FR-604)

| axis | value | source of truth |
|---|---|---|
| implementation progress | `dev_stage` | implements task links (unchanged from M1) |
| verification | `state` | derived from recorded runs |
| approval | `approval`: `draft` \| `review` \| `approved` (M3, wiki/270 §2.3) | `SubItem.approval` when present (authoritative); else the M2 read-mapping of `SubItem.status` (`verified` → `approved`, else `draft`) |

**Approval workflow (M3, wiki/270-vmodel-m3-design.md §2.3, FR-406)**:
`trace_update(set.approval=...)` drives the 3-value lifecycle —
`draft → review` (anyone may propose), `review → approved` (recommended to
be a human action; no technical gate enforces this), and the direct
`draft → approved` shortcut. A `review → approved` or `draft → approved`
transition stamps `approved_hash` (the item's current `def_hash`),
`approved_by` (`executor_id`), and `approved_at`, and writes one audit file
under `.handoff/trace/approvals/<id>.json`. **Automatic rollback**: if a
layer sync later detects the item's `def_hash` changed (the body text or
acceptance criteria were edited), `approval` is reset to `draft`
automatically — `approved_hash` is *not* cleared, so it still reads as "the
hash as of the last approval". Once `approval` has been written at all by an
M3 binary, it is the sole authority for that item — `status`/`reviewer`/
`verified_at` are no longer read or written by the new path (kept only for
M2 binary compatibility).

`trace_update`'s `set` op writes `dev_stage`/`approval`/`impl_refs` only.
`doc_verify(check/check_all/set_dev_stage)` still works (NFR-001) but a
`check`/`check_all` call against a layer document returns a warning that
layer documents don't use it for aggregation.

### 5.2 partial / waived / na (wiki/260 §3.1)

Classification priority (first match wins, one value per axis):
`na` (layer not in use) → `covered` → `partial` → `waived` → `uncovered`.

- **Horizontal partial**: the item has at least one verifier, has acceptance
  criteria, and at least one AC is *not* covered by any verifier (a
  `verifies: REQ-003#AC2` sub-reference, a `from: REQ-003#AC2` scaffold
  item, or the implicit AC item all count as covering that AC;
  `verifies: REQ-003` — the whole item — counts as covering every AC).
- **Vertical partial ("deep coverage")**: a refining child exists, but that
  child (or one of *its* descendants) is itself `uncovered`/`partial`.
  `waived`/`na` children are excluded from this check (a waived child counts
  as covered for its parent's vertical check).
- **Waived**: a `- waive-verify:`/`- waive-refine:` exemption applies **only**
  when the axis would otherwise be `uncovered` — it never downgrades an
  already `covered`/`partial` axis (lint `redundant_waiver`, info, flags
  that case instead). A waiver on an axis that is `na` (layer not used at
  all) is nonsensical — lint `waiver_on_na` (warning).
- **Derived**: a `- derived:` item never reports the `orphan` gap.

Percentages use `covered / (total − na − waived)` everywhere (NFR-005) —
use the same formula if you render your own summary.

### 5.3 Three suspect kinds (wiki/260 §3.2, FR-401/402)

| kind | what | condition |
|---|---|---|
| `link` | a child's `refines`/`verifies` reference | baseline exists and no longer matches the upstream's current hash |
| `task` | a task's requirement link | `baseline_hash` exists and no longer matches the item's current `def_hash` |
| `result` | a verification item's latest recorded result | latest is `pass`, and the recorded hash (`def_hash`, or `body_hash` for a pre-M2 run) no longer matches current |

The spread **stops at one hop** — clearing/ignoring a suspect never
propagates further down; a downstream item only becomes suspect once *its
own* upstream (which may itself already be suspect) actually changes.
`state` itself never changes because of a suspect (a passing result stays
"passing"; suspect/`reverify` are separate flags, §11 Q1).

**`reverify`**: a `passing` verification item needs re-running when its own
result is suspect, or one of its `verifies` links is.

### 5.4 Tool: `handoff_trace_suspect`

```
handoff_trace_suspect(action: "list", item?, task_id?, kinds?: ["link"|"task"|"result"], layers?, limit?: 50)
-> {suspects: [...], counts, unbaselined: {links, tasks}, reverify: [...], truncated}

handoff_trace_suspect(action: "clear", targets: [...], reason: string, evidence?, executor_kind?, executor_id?)
-> {cleared: {links, tasks, results}, clear_id, warnings}

handoff_trace_suspect(action: "baseline", dry_run?: true, scope?: {doc} | {layer})
-> {baselined: {links, tasks, results: 0}, dry_run, warnings}
```

- `targets` (for `clear`): one exact link (`{item, upstream}`), every
  suspect link of one item (`{item}`), every link pointing at one upstream —
  a bulk clear (`{upstream}`), one task's requirement link(s) (`{task_id,
  item?}`), every suspect in one layer (`{layer}`), or one result suspect
  (`{result: item}`). Multiple targets in one call are unioned.
- `clear` writes one audit file per call to `.handoff/trace/clears/<id>.json`
  (git-managed) — `reason` is required.
- `list`/`baseline(dry_run=true)` are read-only (E6). `clear`/
  `baseline(dry_run=false)` write and resync directly-edited layer docs
  first (R-05) so the baseline/audit always reflects current text.

## 6. Tool selection table

| need | tool |
|---|---|
| Coverage/gap report across the whole graph, refresh `_trace_report.json` | `trace_report` |
| Neighborhood around one task/item (saves context) | `trace_slice` |
| Rule-based lint (structure, drift, tailoring, format) with CI exit codes | `trace_lint` |
| Flat CSV/Markdown export (tree or edge list) | `trace_matrix` |
| Ranked "what to do next" across the graph | `trace_next` |
| "What would happen if I changed this?" before editing | `trace_impact` |
| Suspect/reverify list, clear, or migrate unbaselined links | `trace_suspect` |
| Ingest a cargo/JUnit test run and record matched results | `trace_ingest` |
| Generate verification items from acceptance criteria | `trace_scaffold` |
| Bulk edit items/links/runtime fields/results/suspect-clears in one call | `trace_update` |
| Generate tasks for items missing their implements/executes task | `trace_tasks` |
| Propose a new item before writing it (duplicate check + template) | `trace_propose` |
| Record one execution result (single call, not bulk) | `trace_record` |
| View an item's change history (run timeline) | `trace_history` |

All of `trace_report`/`trace_slice`/`trace_lint`/`trace_matrix`/`trace_next`/
`trace_impact`/`trace_suspect(list|baseline dry_run)`/`trace_propose`/
`trace_tasks(preview)` are read-only (E6) — they never resync a
directly-edited layer document to disk, never write derived caches, and
(with the single exception of `trace_matrix`'s own `output_file`) never
write anything else. Every one of the five `trace_update` CLI/MCP names also
exists as a CLI subcommand: `handoff-mcp trace report|record|slice|history|
suspect|impact|lint|matrix|propose|tasks|update`.

## 7. Templates (FR-803)

One starter body per commonly-used layer — start a new layer document from
the matching shape rather than inventing heading syntax from scratch.

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

**Detailed spec** (`layer="detailed_spec"`, left, pairs with `unit_test`):

```markdown
# <Feature> Detailed Spec

### DS-001 <short design item title>

- refines: SPEC-001

<Interfaces, data structures, edge cases at implementation granularity.>
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

**Unit test** (`layer="unit_test"`, right, verifies `detailed_spec`):

```markdown
# <Feature> Unit Tests

### UT-001 <short check title>

- verifies: DS-001
- method: auto
- test: src/<module>.rs::<test_fn>

<What the unit test asserts.>
```

Save each with `handoff_doc_save(slug=..., title=..., layer=..., body=...)`
— `layer` is what turns the document into a layer document; everything else
is ordinary `doc_save`.

### Minimal profile — smallest possible configuration

- Two documents, `layer="requirement"` and `layer="acceptance"`, acceptance
  items `verifies:` the requirement ids. Or:
- **Requirement-only with inline verification (layer-skip)**: a single
  `layer="requirement"` document where an item carries its own `test:`/
  `method:` attribute — that item is *both* the requirement and its own
  verification item. Fastest way to close the V without a second document.
- Or rely entirely on `受入基準:` blocks under `minimal`/`bugfix` — every
  acceptance criterion gets its own implicit verification item for free
  (§3 above); write nothing beyond the requirement document itself.

### Bugfix profile — smallest possible configuration

`bugfix` renames the built-in display names (requirement → 再現条件,
acceptance → 回帰テスト) but keeps `minimal`'s layer set and
`implicit_acceptance: true`. One document is enough:

```markdown
# <Bug> 再現条件・回帰テスト

### FR-900 <short reproduction title>

- priority: P1

<reproduction steps as the requirement statement>

受入基準:
- AC1: <the specific regression check that must now pass>
```

## 8. AI layer-skip development flow (FR-1006)

1. Write a `basic_spec` item with a `test:` attribute (inline verification),
   or a `requirement` item with a 受入基準 block under a minimal-shaped
   profile.
2. `handoff_update_task(task={id, requirement_ids: [stable_id, ...]})` to
   link your task to it.
3. Implement, run the test.
4. Record the result in bulk with the rest of your changes:
   `trace_update(ops: [{op: "record", item: "...", result: "pass", ...}, ...])`,
   or ingest a whole test run with `trace_ingest`. Use the single-shot
   `trace_record` only when you are recording in isolation, not as part of a
   larger batch of edits.
5. Before marking the task done, check `trace_next(task_id: "...")` for
   remaining blockers (failing/blocked verifiers, suspect links, items still
   missing a verifier).

## 9. Getting test results in (`trace_ingest`, wiki/260 §4.6)

```
handoff_trace_ingest(format: "cargo_json"|"junit_xml", output? | output_file?, commit?, task_id?, executor_kind?: "ai", dry_run?: false)
-> {run_id?, recorded, matched: [{item, result, tests}], missing_refs, unmatched_tests_count, warnings, dry_run}
```

**Getting cargo output — the correct commands:**

- `cargo test --format json` **does not exist** on stable — libtest's
  JSON-per-line output is an unstable feature. If you need it anyway:
  `cargo +nightly test -- -Z unstable-options --format json`, or on stable
  `RUSTC_BOOTSTRAP=1 cargo test -- -Z unstable-options --format json`.
- **Recommended**: `cargo nextest run` with a `[profile.<name>.junit]`
  section in `.config/nextest.toml` writes a JUnit XML report with **no
  unstable flags at all** — feed that to `trace_ingest(format: "junit_xml")`.

Matching (3 stages, tried in priority order): (1) exact match against a
declared `- test: <value>`, (2) a `::`-boundary suffix match, (3) the pre-M2
`stable_id` → test-name-prefix convention. An item with at least one
declared `test` value is recorded only when *every* declared value is
covered by this ingestion's output — a missing one goes to `missing_refs`
instead of silently marking the item `not_run`/overwriting a fuller run.

`handoff_doc_req_test_sync`, the pre-M3 cargo-JSON-only tool this ingestion
path superseded, was removed at the M3 release (wiki/270-vmodel-m3-design.md
§4.8) — use `handoff_trace_ingest(format="cargo_json")` for that input
format instead.

## 10. Bulk updates (`trace_update`, wiki/260 §4.8)

```
handoff_trace_update(ops: [
  {op: "upsert_item", doc, id, title?, statement?, acceptance?: [{label,text}], attrs?: {...}, after?},
  {op: "link" | "unlink", item, task?, role?},
  {op: "set", item, dev_stage?, approval?, impl_refs?, priority?, test_refs?},
  {op: "record", item, result, note?, evidence?},
  {op: "clear_suspect", item?, upstream?, task_id?, layer?, result?, reason: "..."},
], task_id?, dry_run?: false, executor_kind?: "ai", executor_id?, commit?)
-> {applied: [{op_index, op, result}], failed?: {op_index, error}, warnings, suspect_introduced?}
```

All ops are validated (document/item/task existence, enum values) **before
anything is written** — if any op fails validation, nothing is written.
`dry_run: true` previews everything, including a unified-diff hunk per
`upsert_item`. Writing a new/changed `derived`/`waive-verify`/
`waive-refine` always adds a `waiver_added: <id> <axis> <reason>` warning —
**do not add an exemption without the user's confirmation**; review the
warning before relying on an AI-authored waiver (E4).

If a call partially fails, retry only the ops **not** in `applied` —
resending the whole array double-records `record`/`clear_suspect` audit
entries.

## 11. Human V-model operating flow (FR-1005)

Each stage below names the handoff-vscode screen (wiki/100-vmodel-ui-design.md
§2.1, in the handoff-vscode repo) and the MCP tool/CLI a human uses at that
stage.

| stage | VSCode screen | MCP tool / CLI |
|---|---|---|
| 要件起票 (raise a requirement) | ドキュメント（文書一覧・本文） | `doc_save(layer="requirement")`, or `trace_propose` to check for a near-duplicate first |
| 仕様展開 (expand into specs) | ドキュメント | `doc_save(layer="basic_spec"/"detailed_spec", ...)` with `refines:` |
| 検証設計 (design verification) | ドキュメント / トレース | `trace_scaffold` (from acceptance criteria) or hand-write an `acceptance`/`system_test`/`unit_test` item with `verifies:` |
| タスク化 (turn into tasks) | 計画（ボード） | `trace_tasks` (bulk, from gaps) or `handoff_update_task(requirement_ids=[...])` (one at a time) |
| 実装 (implement) | エディタ / Inspector | `handoff_update_task` status transitions; `trace_slice(task_id)` for context |
| 検証実行 (run verification) | Inspector（セッション） | `trace_ingest` (CI/test run) or `trace_update`'s `record` op |
| 承認 (approve) | Inspector（要件・項目） | `trace_update`'s `set` op (`approval: "approved"`), or `doc_verify(set_dev_stage)` pre-M2 |

Cross-cutting, at any stage: トレース画面 for the V-model table/graph/matrix
(`trace_report`/`trace_matrix`), suspect review and clearing
(`trace_suspect`), and "what's next" (`trace_next`).

## 12. VSCode write paths (wiki/260 §5.4)

- Suspect clear ("確認して解除") → `trace_suspect(action="clear")`, never a
  direct baseline write from the TS side (keeps the document/task/audit-file
  triad consistent).
- Matrix export → CLI `trace matrix`, not a TS-side CSV/Markdown
  reimplementation (NFR-005).
- Moving a task to review/done through the Kanban/Inspector bypasses the MCP
  done-guard (§13 below) because the TS writer edits the task file directly
  — VSCode must check `_trace_report.json`'s `tasks[].blockers` itself before
  the move, and (when `done_guard = "block"`) route the move through CLI
  `task update` instead of the TS writer.

## 13. Done guard and task blockers (wiki/260 §3.4)

`[trace] done_guard` (`warn` default | `block` | `off`): when
`handoff_update_task` moves a task's status to `review`/`done`, and that
task has a requirement link with a blocker (an unexecuted/failing/blocked
verifier, or a `reverify` one, on anything it implements/executes) —
`warn` appends a warning, `block` rejects the call unless `force: true`,
`off` does nothing. A task with no requirement link is never affected.

## 14. Lint rules reference (wiki/260 §4.3)

```
handoff_trace_lint(rules?: [string], fail_on?: "error"|"warning" = "error", format?: "json"|"text" = "json", limit?)
-> {findings: [{rule, severity, item?, task?, doc?, message}], counts, exit_code, warnings}
```

| category | rules | default severity |
|---|---|---|
| structure (M1 gaps) | `unverified`, `unrefined`, `orphan`, `task_unlinked` | warning |
| | `dangling`, `invalid_link`, `cycle`, `duplicate_id` | error |
| change | `suspect_link`, `suspect_task`, `stale_result` | warning |
| | `unbaselined` | info |
| tailoring | `waiver_on_na` | warning |
| | `unlabeled_acceptance`, `invalid_waiver`, `unknown_acceptance_ref` | warning |
| | `redundant_waiver`, `layer_outside_profile` | info |
| drift | `unsynced_body`, `task_link_dangling`, `attribute_after_body` | warning |
| | `task_ids_drift`, `orphaned_legacy`, `orphan_run`, `id_like_heading` | info |
| format | `frontmatter_invalid` | error |

`attribute_after_body` (M3, t377.5): a `- priority: ...`/`- assignee: ...`
etc. bullet line that was written *after* the item's body text instead of
in the attribute block right after its heading — never applied as an
attribute, silently lost otherwise. See §3's "Attribute line placement"
warning below for the full explanation and copy-pasteable per-layer
templates.

Project policy rules via `[[trace.lint.require]]`:

```toml
[[trace.lint.require]]
id = "p0-needs-verification"
when = { layer = "requirement", priority = ["P0", "P1"] }
need = "verified_by"   # verified_by | refined_by | implemented_by_task | passing | no_suspect | auto_test
severity = "error"
```

CLI exit codes (distinct from every other `handoff-mcp` subcommand's generic
0/1): `0` = no finding at/above `fail_on`, `1` = at least one finding,
`2` = usage/config error (invalid flag, malformed `config.toml`, unknown
rule id in `rules`/`require`).

## 15. Migrating from req_* SubItems to a V-model layer document (t377.12)

If you already imported requirements with `doc_req_import` / tracked them with
`doc_req_list` / `doc_req_status`, and now want this document's requirements to
live in a V-model layer instead (so `trace_report`/`trace_slice`/`trace_next`/
`trace_lint` all work on it), the two representations are **not** interchangeable
in place — `req_*` SubItems live in the document's `verification` matrix as
freeform items; a layer document's SubItems are parsed from the Markdown **body**
itself (see §3 Body notation). Converting means re-authoring the body, not
flipping a flag.

### Why they're mutually exclusive on the same document

Once `doc_save(layer=...)` is set, `handoff_doc_verify`'s `add_item`,
`set_priority`, `backfill_stable_ids`, and `set_refs` (when the call includes
`test_refs`) are refused — the error tells you to edit the body instead,
because those fields are now owned by the Markdown body and would be
overwritten by the next body sync. Symmetrically, `handoff_doc_req_import` is
refused outright on a layer document. Trying either direction without first
deciding which representation this document owns is the most common dead end.

### Migration steps (req_* -> layer)

1. **Inventory what you have**: `handoff_doc_req_list(task_id=<this document's id>)`
   or filter by `doc_id` logic (there is no `doc_id` filter on `req_list` directly —
   use `handoff_doc_get(doc_id, format="meta")`'s `verification.items` to list
   the document's current freeform SubItems: stable_id, title, priority,
   dev_stage, impl_refs, test_refs).
2. **Pick a layer and profile** (§1, §2) that matches what these requirements
   actually are — most `req_import`-ed specs map to `req` (FR-xxx) or `spec`
   (SPEC-xxx).
3. **Re-author the body** using the layer's template (§7) — one heading per
   SubItem, in the exact heading -> attribute block -> body order (§3's
   "Attribute line placement"). Carry over each existing SubItem's priority,
   dev_stage, impl_refs, and test_refs into the new attribute block; carry the
   stable_id forward unchanged if you want traceability history to survive
   (dangling-link lint rules match on stable_id, not on creation order).
4. **Call `doc_save(layer=..., trace_profile=...)`** with the rewritten body.
   This replaces the freeform `req_*` items with body-derived SubItems in one
   shot (no partial/manual state).
5. **Verify nothing was lost**: run `handoff_trace_lint` on the document and
   `handoff_trace_matrix` to confirm every stable_id you carried over resolves
   and every `impl_refs`/`test_refs` pair the import had is still attached.
6. **Downstream**: anything that queried this document via `handoff_doc_req_list`
   /`doc_req_status` keeps working unchanged — layer-document SubItems with a
   stable_id are included in `req_list`'s output on equal footing with freeform
   ones (both are read from the same `verification.items[].sub_items` field).
   What changes is *how you edit* the document going forward, not how it's
   queried.

### When NOT to migrate

If this document is a one-off spec with no plan to track `verified`/`stale`
state over multiple runs, `req_import` + `req_status` alone is sufficient and
migrating adds no value — layers exist for documents that need lint/suspect/
baseline/diff tracking across changes (§5, §8).

## 16. Diagnostics handling

`trace_report`, `trace_slice`, `trace_next`, and the rest of the `trace_*`
family return a `warnings` array that may contain structured entries
(`{severity, code, message, fix_hint?}`) alongside plain strings — see
`handoff/SKILL.md`'s "Diagnostics & Warnings Handling" for the general
severity rule. This section covers the trace-specific codes.

### DIAG-T001 / T002 / T003 (trace graph diagnostics)

These three fire when `trace_report` (or another trace tool that builds the
same graph) finds the effective used-layers set empty or unusable — the
situation that otherwise shows up only as a silently empty report:

| code | condition | fix |
|---|---|---|
| `DIAG-T001` | `[trace]` is not configured at all (no explicit `layers`, no `profile`, and auto-detection found nothing) | Set `[trace] layers = [...]` or `[trace] profile = "..."` in `config.toml`, or create at least one layer document |
| `DIAG-T002` | the used-layers set resolved, but zero documents have `layer` set | `handoff_doc_save(doc_id=..., layer="requirement")` on at least one existing document, or write a new one from §7's templates |
| `DIAG-T003` | one or more `_doc.<slug>.md` files failed to parse (unreadable), so their items are missing from the graph | `handoff_doc_repair_frontmatter(dry_run=true)` to see what's recoverable, then `dry_run=false` to fix it |

Each of these carries its own `fix_hint` with the exact call to make —
prefer that over improvising, since the hint is generated from the same
data the diagnostic itself inspected.

### Empty-result triage

An empty `trace_report`/`trace_slice`/`trace_next` result (no items, no
findings, no actions) has two very different explanations — **check
`warnings` first** before concluding "there is nothing to do":

1. **Genuinely nothing to do** — no `warnings`, or only `info`-level ones.
   The graph is healthy and simply has no gaps/suspects/actions right now.
2. **Diagnostic condition** — a `DIAG-T00x` (or project-specific lint
   `require` rule) explains why nothing was found: no layers configured, no
   layer documents yet, or an unreadable document hid the real data. Treat
   this as a setup problem to fix, not as "the project has no requirements".

### `trace_lint` findings vs. `warnings`

Don't conflate the two: `trace_lint`'s `findings` array (rule/severity/item/
message) is the **content-level** result the tool is for — report it the
same way regardless of this section. `trace_lint`'s own `warnings` field (if
present) is about the **call itself** (e.g. an unrecognized `rules` filter
entry) and follows the general severity handling above.

## See also

- `handoff-docs` SKILL.md — the generic `doc_*` tools (save/get/list/...)
  layer documents are built on top of.
- `handoff` SKILL.md — session start/end and task tracking.
- `plugin-task-loop/commands/session-loop.md` — how session-loop consumes
  `trace_slice`/`trace_propose`/`trace_next`/`trace_update` across a session.
