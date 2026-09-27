# Pre-M0/M1 compat fixture

t360.12 (wiki/220-vmodel-integration-design.md §5/§7, NFR-001/002). `project/handoff/`
(no leading dot, like `tests/fixtures/trace/project/handoff/` — the repo's
blanket `.handoff/` `.gitignore` rule would otherwise swallow a checked-in
fixture literally named `.handoff/`) is a frozen `.handoff/` project directory
produced by the real `handoff-mcp`
binary built from the actual `main` branch (commit `7aa83323966a2413a93f4cc404c5e633a82964e3`,
2026-09-26 — i.e. genuinely *before* any M0/M1 work landed, not just "before
this task"), covering the three pre-existing shapes M0/M1 must not disturb:

- **A layer-less document** (`req-c01-legacy-spec` — no `layer` frontmatter
  key at all).
- **A `C{n}`-prefixed section-attached `SubItem`** (`C01-FR-001`, created via
  `handoff_doc_verify(add_item, fragment_seq=1, description="FR-001: ...")`,
  then `check`ed and `set_priority`'d — same `derive_stable_id` category-prefix
  path pre-existing projects rely on).
- **A freeform `SubItem`** (`req-c02-board-setup`'s `C02-2.1.1.1`/`C02-2.1.1.2`
  — items living inside a `VerificationItem` with `fragment_seq: null`, the
  shape `handoff_doc_req_import` produced for every matrix-less document
  before the M0 FR-806 fix. `main` itself already has this shape; the FR-806
  fix changed what `req_import` produces *going forward*, not how an
  already-frozen project like this one reads back — the point of this fixture
  is exactly that reading it must not have changed).

`expected_output.json` is `{req_list, req_status, verify_status_a,
verify_status_b}` captured **byte-for-byte** from that same `main` binary run
over this exact project. `tests/pre_m1_compat_e2e.rs` copies `project/`
into a tempdir as-is (never re-runs `handoff_doc_save`/`handoff_doc_req_import`
against it — see "Deliberately excluded" below) and only calls **read** tools
(`handoff_doc_req_list`, `handoff_doc_req_status`, `handoff_doc_verify_status`)
against the current `handoff-mcp` binary, so every value that would otherwise
be wall-clock-volatile (`doc_id`, `verified_at`, `created_at`/`updated_at`) is
instead whatever was already frozen into the copied `.handoff/` files — the
current binary's JSON response for a pure read must reproduce those same
frozen values exactly, with **zero** normalization needed.

## Deliberately excluded from this comparison

- **`handoff_doc_req_status`'s `items[].category` field.** `aggregate_requirements`
  (the function backing both `_requirements_summary.json` and
  `handoff_doc_req_status`) gained a `category` field on every item as part of
  the same intentional M1 change referenced below (wiki/220 §2.7: "summary の
  items に layer, category を追加する") — purely additive (a new key, nothing
  removed or renamed), and it leaks into `req_status`'s live response because
  it shares the same struct as the derived summary file. `expected_output.json`
  has `"category": "requirement"` added to every `req_status.items[]` entry
  (all fixture items are left-side/no-layer requirements) to reflect this;
  every other field, and `handoff_doc_req_list`'s output (a distinct struct
  that was *not* given a `category` field), is compared with zero
  normalization.
- **`.handoff/docs/_requirements_summary.json`'s on-disk format.** The
  checked-in fixture still carries the pre-M1 pretty-printed shape (no
  `inputs` fingerprint, `category: "check"` items not excluded from
  aggregates) because it was produced by the `main` binary — this is an
  intentional M1 format change (unformatted JSON, `inputs` fingerprint,
  `category == "check"` exclusion; see `wiki/220-vmodel-integration-design.md`
  §4.3 and the CHANGELOG's `[Unreleased]` section), not a compat regression to
  guard against. The test never reads this file directly.
- **`req_import`'s own *write-time* behavior** (whether a fresh `req_import`
  call bootstraps a freeform item or places items into sections) — that
  changed intentionally in M0 (FR-806, already covered by
  `tests/tool_doc_req_import.rs`'s
  `req_import_then_update_task_requirement_ids_links_successfully`). This
  fixture captures the freeform shape as a frozen *pre-existing* on-disk
  artifact and asserts only that **reading** it back is unaffected — it does
  not re-run `req_import` against the current binary.

## Legacy task/link shape (t360.42 N7)

`tasks/t-legacy-legacy-task/` is a hand-authored (not `main`-binary-produced)
task `t-legacy` carrying a pre-M1-shaped `TaskLink{link_type:"requirement",
label:"C01-FR-001"}` with **no `role` key at all** — the exact shape a link
written before M1 introduced `role` (wiki/220-vmodel-integration-design.md
§2.5) would have on disk. `C01-FR-001`'s `SubItem.task_ids` in
`_doc.req-c01-legacy-spec.md` is correspondingly seeded with `["t-legacy"]`
(task-linking predates M1; only `role` is new). `expected_output.json`'s
`req_list`/`req_status`/`verify_status_a` entries for `C01-FR-001` were
updated to include this `task_ids`/`task_coverage` value — task_ids is a
pure pass-through on every read path these tools exercise (no M0/M1 read-side
transform touches it), so computing the "expected" value via the *current*
binary here is equivalent to what a pre-M1 binary would have produced.

`tests/pre_m1_compat_e2e.rs`'s
`doc_save_and_update_task_do_not_corrupt_a_legacy_role_less_task_link` is the
only test in this file that *writes*: it resaves the legacy document
(`handoff_doc_save`) and re-supplies `t-legacy`'s unchanged `requirement_ids`
(`handoff_update_task`), asserting neither corrupts the legacy
`task_ids`/`task_links` link, and that the S7 backfill
(`backfill_missing_requirement_link_roles`) persists an inferred `role`
(`"implements"`) onto the previously role-less link.
