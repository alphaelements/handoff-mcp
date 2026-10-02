# v2 contract fixture (M2-07, wiki/260-vmodel-m2-design.md §5.1)

See `tests/fixtures/trace/README.md` for the shared conventions (why this
directory exists, formatting, write-trigger, `inputs` fingerprint) — this
file only documents what is specific to `v2/project/handoff/`.

## What this fixture covers

Config (`project/handoff/config.toml`): `[trace] profile = "standard"` (the
project default) plus a custom, mutually-paired layer declaration
(`[[trace.layer]]` `risk`/`mitigation` — side left/right, level 1, id
prefixes `RISK`/`MIT`) that is declared but not used by any item, purely to
exercise `layer_defs[]` carrying a project-defined (non-built-in) layer
alongside the 6 built-ins.

- **`bugfix-doc` document** (`trace_profile: bugfix` override, §2.1 規則 1):
  `REQ-001` with two acceptance criteria (AC1/AC2). `bugfix`'s
  `implicit_acceptance: true` materializes `REQ-001#AC1`/`REQ-001#AC2` as
  real SubItems (`implicit_of: "REQ-001"`) — covers `items[].implicit_of`
  and `items[].acceptance`. `profile.overrides[]` carries this document with
  its `display_names` (`requirement` → "再現条件", `acceptance` → "回帰テスト",
  §2.1's table) — the only built-in profile with display-name overrides.
  `REQ-001.profile == ["bugfix"]`, demonstrating per-document profile
  tree-inheritance distinct from the project default.
- **`requirements-v2` document** (project default `standard` profile):
  - `REQ-002` (2 ACs) — `acceptance-v2`'s `AT-002` verifies only
    `REQ-002#AC1` (an explicit sub-reference, §2.2), leaving AC2
    unaddressed: `REQ-002.coverage.horizontal == "partial"` (§3.1).
  - `REQ-003`, refined by `basic-spec-v2`'s `SPEC-003`, which nothing
    verifies: `SPEC-003.coverage.vertical == "uncovered"` folds up into
    `REQ-003.coverage.vertical == "partial"` (deep coverage, §3.1/§11 Q7).
  - `REQ-005` carries `- waive-verify: ...` with no verifier:
    `REQ-005.coverage.horizontal == "waived"`, `items[].waivers ==
    [{axis: "verify", reason: "..."}]`.
  - `REQ-007`, verified (whole-item reference, not an AC sub-reference) by
    both `AT-006` and `AT-007` — the baseline/suspect/unbaselined
    scenarios below all hang off this one requirement.
- **`acceptance-v2` document**:
  - `AT-002` (verifies `REQ-002#AC1`) and `AT-006` (verifies `REQ-007`) each
    had a `pass` recorded, then two further edits were made (in this order):
    (1) `REQ-002`'s AC1 text and `REQ-007`'s body text both changed, (2)
    `AT-006`'s own body text changed. Net effect:
    - `AT-002`: **no** suspect at all — its upstream reference is the AC
      sub-reference `REQ-002#AC1`, and `REQ-002`'s document uses the
      `standard` profile (`implicit_acceptance: false`), so there is no
      materialized `REQ-002#AC1` SubItem to resolve a "current hash" from;
      per wiki/260 §3.2's suspect-resolution note, an AC-level reference
      whose implicit item doesn't exist resolves to "unknown", which is
      deliberately neither a suspect nor `unbaselined` (false negatives
      preferred over false positives here). This is intentional fixture
      coverage of that specific gap, not an oversight — don't "fix" `AT-002`
      into a suspect by changing `requirements-v2`'s profile.
    - `AT-006`: a **`link`** suspect (upstream `REQ-007` changed), a
      **`result`** suspect (its own recorded-pass `def_hash` no longer
      matches its own current `def_hash`, since its body changed after the
      pass was recorded), and `reverify: true` (passing + either suspect
      kind qualifies, §3.2). Also a **`task`** suspect on `t-exec-webhook`
      (which `executes` `AT-006`, baselined on `AT-006`'s own `def_hash`).
    - `AT-007`: see "the one hand-edit" below — ends up `unbaselined`
      instead of a `link` suspect.
    - `t-impl-throttle` (`implements REQ-002`): a **`task`** suspect
      (`REQ-002`'s `def_hash` changed along with its AC1 text).
  - `ST-004` carries `- derived: ...` with no `verifies` — covers
    `items[].derived`, and that `derived` items are excluded from the
    `orphan` gap.
- **Tasks**: `t-impl-throttle` (`implements REQ-002`), `t-exec-throttle`
  (`executes AT-002`), `t-exec-webhook` (`executes AT-006`) — exercise
  `tasks[].layers` (grouped by `{layer, role}`) and `tasks[].blockers`
  (`t-exec-webhook` shows `reverify: 1, suspect: 1`, from `AT-006` above).

## The one hand-edit: `AT-007`'s `unbaselined` link

Every other field in this fixture's `_doc.*.md` files is exactly what the
real binary wrote — per the parent README's rule, nothing here is
hand-computed. `AT-007` (`verifies: REQ-007`, same whole-item reference
`AT-006` uses) is the single, deliberate exception: after generating the
project through the real binary, `_doc.acceptance-v2.md`'s `AT-007` SubItem
had its `link_baselines: {REQ-007: ...}` entry **deleted by hand**, and
`expected_output.json` was then regenerated from the real binary run against
*that* edited project (so `expected_output.json` itself is still 100%
binary-generated — only the input `AT-007` SubItem's `link_baselines` map
was touched).

This is necessary, not a shortcut: wiki/260 §2.5 step 4 baselines **every**
newly-added `refines`/`verifies` reference immediately, the moment a body
containing it is synced — there is no sequence of live `handoff_doc_save`/
`handoff_trace_*` calls that leaves a reference to a real, already-`def_hash`'d
item without a baseline. `unbaselined` only exists for data that predates
M2 entirely (§7: "M1 のデータで M2 を使い始めたとき... 既存のリンクはベース
ラインがない") — there is no M1 binary left to generate such a project with,
so this one hand-edit simulates exactly that documented scenario. Confirmed
in the regenerated `expected_output.json`: `AT-007`'s `suspect: []` (an
unbaselined link is never a suspect, §3.1/§5.4) and `suspect_counts.unbaselined
== 1`.

## Regenerating this fixture

1. Reproduce (or extend) the scripted sequence of `handoff_init`/
   `handoff_doc_save`/`handoff_update_task`/`handoff_trace_record` calls
   against a throwaway project directory, driving the real `handoff-mcp`
   binary over stdio JSON-RPC (the same pattern
   `tests/trace_report_contract_fixture_e2e.rs`'s own `Server` helper uses).
2. Copy the resulting `.handoff/` into `project/handoff/` here (minus the
   derived files the parent README says to omit: `docs/_trace_report.json`,
   `docs/_task_ids_rebuild.json`, `runs/_latest.json`).
3. Re-apply the `AT-007` `link_baselines` deletion documented above.
4. Copy `project/handoff/` into a fresh tempdir as `<tmp>/.handoff` and run
   `handoff-mcp trace report --project-dir <tmp>` against it — this is the
   authoritative regeneration step. Pretty-print its
   `.handoff/docs/_trace_report.json` into this directory's
   `expected_output.json`.
