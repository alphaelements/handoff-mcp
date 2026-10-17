# Change tracking: hashes, baselines, suspects, impact, proposals

How the server notices that a definition changed, what it flags as a result,
and how to review, clear, preview or propose changes. This is a workflow
guide. Full argument lists and response shapes are in `tools.md`.
Configuration is in `config.md`. Recording results, approval and the done
guard are in `verification.md`.

Example calls show the tool name followed by its JSON arguments.

## 1. Item hashes

Each layer item carries three hashes. They are recomputed whenever its
document is synced (section 2).

| hash | input | used for |
|---|---|---|
| `def_hash` | title + statement + acceptance criteria (as `label:text`, in order) + branch labels | link/task/result suspects, approval reset, delta staleness, baseline diffs |
| `ac_hash` (one per acceptance criterion) | title + statement + branch labels + **this** criterion's text | `verifies: X#ACn` references, implicit `X#ACn` items |
| `body_hash` | title + statement + the attributes `layer`, `refines`, `verifies`, `priority`, `method`, `test` | fallback for runs recorded before `def_hash` existed |

Details for `def_hash` and `ac_hash`:

- The text is reduced to plain text first. Markdown emphasis and line
  wrapping are dropped, then NFKC normalization is applied and whitespace
  runs are collapsed. Reformatting alone does not change the hash.
- **Attributes are excluded.** `refines`, `verifies`, `priority`, `method`,
  `test`, `parent`, `order`, `kind`, `needs`, `assignee`, `rationale`,
  `derived`, `from` and the waivers never change `def_hash`. Re-pointing a
  link or changing priority does not make anything suspect.
- **Branch labels are included.** `- condition:` and `- condition@<parent>:`
  are part of the definition. Changing "yes" to "no" makes the item's
  verifiers suspect.
- `ac_hash` leaves out the other criteria and the label. Editing `AC2` never
  changes `ac_hash(X, AC1)`. Renumbering criteria changes `def_hash`, though.
- An implicit acceptance item `REQ-003#AC1` has `def_hash` equal to the
  parent's `ac_hash` for `AC1`.

`body_hash` is only whitespace-normalized, and it does include those six
attributes. A run is compared by `def_hash` when it recorded one. Otherwise
it is compared by `body_hash`.

## 2. When documents are synced

A "sync" re-parses a layer document's Markdown body and rebuilds its item
metadata: hashes, links, acceptance criteria and implicit items. A
**writing** sync saves the result. An **in-memory** sync is used by
read-only tools and is discarded afterwards.

| call | sync behavior |
|---|---|
| `handoff_doc_save`, `handoff_doc_update_section` | writing sync of that document. Also forced by a change of `layer`, `split_level` or `trace_profile`, even with an unchanged body |
| `handoff_trace_update` `upsert_item` | writing sync of every touched document, batched |
| `handoff_doc_verify` `action: "sync"` on a layer document | writing sync of that document (see the caution below) |
| `handoff_trace_report`, `handoff_trace_slice` | writing resync of **every** layer document edited directly since its last sync, before aggregating |
| `handoff_trace_suspect` `clear`, and `baseline` with `dry_run: false` | writing resync of every directly edited layer document first |
| `handoff_trace_record` (and `trace_update` `record` ops) | writing resync of only the documents owning the recorded items |
| `handoff_update_task` adding `requirement_ids` (also `trace_update` `link`, `handoff_trace_tasks` apply) | writing resync of the documents owning the newly linked items, before their hash is captured |
| `handoff_trace_baseline` `create`, `set_layer_status`, `handoff_report` `generate` (all types except `effort`) | the same path as `handoff_trace_report` |
| `handoff_trace_lint`, `handoff_trace_next`, `handoff_trace_matrix`, `handoff_trace_tree`, `handoff_trace_impact`, `trace_suspect` `list` / `baseline` dry-run, `handoff_trace_tasks` scan, `handoff_trace_test_run` `create` | in-memory only. Nothing is written and no baseline is recorded |
| `handoff_trace_ingest`, the `handoff_trace_delta` staleness check, `trace_update` `set` (its `approved_hash` stamp) | no resync; they use the stored hashes |

A document counts as "directly edited" when its raw bytes differ from what
was last synced. That happens after editing in an editor, a `git pull`, or
a branch switch. `trace_lint` reports such documents as `unsynced_body`.
**After editing layer files by hand, call `handoff_trace_report` once**
before recording, ingesting, approving or applying deltas, so stored hashes
are current.

**Config changes:** a sync is also forced, even with an unchanged body, when
any of these change: the layer registry (built-in plus `[[trace.layer]]`),
`[trace.id_prefixes]`, the default `[trace] profile` name, or any profile's
`implicit_acceptance`. This can create or remove implicit `X#ACn` items. The
runs of a removed implicit item stay in `runs/` and show up as lint
`orphan_run`. Changing `[trace] layers`, lint settings, `done_guard` or
display names does not force a sync.

> **Caution:** `handoff_doc_verify` with `action: "sync"` currently skips
> the automatic approval reset (and the assignee roster warning). Changed
> items keep `review`/`approved`, and later syncs see the document as
> already current. To re-sync a layer document after a direct edit, prefer
> `handoff_trace_report`, or re-save with `handoff_doc_save`.

## 3. Link baselines

A `refines` or `verifies` reference stores the upstream's hash at the moment
the link was first seen. That stored hash is the reference's **baseline**.
For a whole-item reference it is `def_hash`; for `X#ACn` it is
`ac_hash(X, ACn)`. A task's requirement link stores the item's `def_hash` as
its `baseline_hash`.

When baselines are recorded:

- **Item links:** at a *writing* sync, for every reference that was not in
  the item's stored `refines`/`verifies` before.
  - A *new* reference to an upstream in the same document uses the hash
    freshly parsed in the same sync. An *existing* link whose upstream you
    edit in the same save still turns suspect — review it as usual.
  - An upstream in another document is resolved against that document's
    current body.
  - An upstream that cannot be resolved (dangling, or no hash yet) gets no
    baseline. The link is *unbaselined*.
  - A reference removed from the body has its baseline dropped.
  - An existing reference's baseline is **never** updated by a sync. Only
    clearing moves it (section 4).
- **A link added by editing the file directly** gets its baseline at the
  next writing sync of that document (for example the next
  `handoff_trace_report`), using the upstream's hash *at that time*.
  In-memory syncs never record baselines.
- **Implicit items** record their own baseline once, when created.
- **Task links:** when the link is added through `handoff_update_task`,
  `trace_update` `link` or `handoff_trace_tasks`.

### Unbaselined links

A link without a baseline is never a suspect. It is counted under
`unbaselined` and appears as lint `unbaselined` (info) and next-action
`baseline`. This applies to links that predate baselining, or links whose
upstream did not exist yet. Fill them in deliberately:

```
handoff_trace_suspect {"action": "baseline", "scope": {"layer": "basic_spec"}}
handoff_trace_suspect {"action": "baseline", "scope": {"layer": "basic_spec"}, "dry_run": false}
```

- `dry_run` defaults to `true`. Read the counts first.
- `scope` is `{doc}` or `{layer}`. Omit it for the whole project.
- Baselining takes the upstream's **current** hash. It asserts "the child is
  consistent with the parent as it is now". Review before running it.
- It never touches a link that already has a baseline.
- An unresolvable upstream stays unbaselined.

## 4. Suspects

| kind | flagged when | typical cause |
|---|---|---|
| `link` | a child's `refines`/`verifies` baseline differs from the upstream's current hash | the parent requirement or spec was edited |
| `task` | a task link's `baseline_hash` differs from the item's current `def_hash` | the item a task implements or executes was edited |
| `result` | an item's latest result is `pass` and the recorded `def_hash` (or `body_hash`) differs from the current one | the verification item itself was edited after it passed |

- **One hop only.** Suspects never propagate. If `REQ-1` changes, `SPEC-1`
  (which refines it) becomes link-suspect. `ST-1` (which verifies `SPEC-1`)
  becomes suspect only when `SPEC-1` itself changes.
- **`reverify`** lists items whose `state` is `passing` and whose own result
  is suspect, or one of whose `verifies` links is suspect. `state` itself
  never changes because of a suspect.
- Suspects feed `handoff_trace_next` (`review_suspect`, `rerun`), lint
  (`suspect_link`, `suspect_task`, `stale_result`) and the done guard
  (`suspect`, `reverify` blockers).
- AC-level references are suspect-checked only through implicit `X#ACn`
  items. Under a profile without implicit acceptance, a
  `verifies: REQ-003#AC2` reference is never reported as suspect. Reference
  the whole item when you need change detection there.

### 4.1 Where suspects appear first

`handoff_doc_save`, `handoff_doc_update_section` and `trace_update`
`upsert_item` return `suspect_introduced` whenever an item's `def_hash`
moved:

```
"suspect_introduced": {"changed": ["REQ-003"],
                       "links": [{"child": "SPEC-020", "upstream": "REQ-003", "type": "refines"}],
                       "tasks": [{"task": "t12", "item": "REQ-003"}],
                       "reverify": ["AT-REQ-003-1"]}
```

It is a cheap summary. It covers only baselined references and leaves out
some fallback cases. `handoff_trace_suspect {"action": "list"}` is the
authoritative list.

The same calls (except `doc_verify`) return `orphaned_children`. These are
requirement-tree children left without any surviving parent because an item
disappeared. A DAG child that still has another parent is not listed.
Decomposition links are not hashed, so children never become suspect. Re-read
them.

### 4.2 Reviewing and clearing

```
handoff_trace_suspect {"action": "list", "kinds": ["link"], "layers": ["basic_spec"]}
```

For each suspect, read the upstream change (`baseline_hash` vs
`current_hash`, `handoff_trace_slice`, git history). Then:

| finding | action |
|---|---|
| child is still correct | clear it, with a reason |
| child must change | edit the child, then clear the link suspect. Editing the child does not move the link's baseline |
| a passing test may no longer hold | re-run and record. The new run carries the current hash and ends the `result` suspect. A `verifies` link suspect still needs a clear |
| task scope changed | review the task (estimate, schedule), then clear the task suspect |

```
handoff_trace_suspect
{"action": "clear",
 "targets": [{"item": "SPEC-020", "upstream": "REQ-003"}, {"task_id": "t12"}],
 "reason": "REQ-003 wording clarified; SPEC-020 unaffected",
 "evidence": {"commit": "a1b2c3d", "note": "reviewed with PO"},
 "executor_kind": "human", "executor_id": "alice"}
```

Target forms (several targets are unioned):

| target | selects |
|---|---|
| `{item, upstream}` | one link |
| `{item}` | every suspect link of that item |
| `{upstream}` | every suspect link pointing at that upstream (bulk) |
| `{task_id, item?}` | that task's suspect link(s) |
| `{layer}` | every suspect whose item is in that layer, all kinds |
| `{result: item}` | that item's result suspect |

- `reason` is required.
- A link or task clear moves the baseline to the current hash.
- A result clear does **not** edit history. It writes a new run that carries
  the last result forward against the current hashes (`carried_from`
  records the original run). Only clear a result when the change cannot
  affect the outcome. Otherwise re-run the test.
- Each call writes one audit file, `.handoff/trace/clears/<clear_id>.json`,
  with the executor, reason, evidence, and every cleared link, task and
  result with its from and to hashes.
- `trace_update` `clear_suspect` ops do the same, one audit file per op.
- `baseline_suggestions` (nothing is written) lists cleared tasks that have
  schedule dates. `set_recommended` means there is no schedule baseline yet,
  `update_recommended` means it differs, and `manual_baseline_detected`
  means a pinned task (do not overwrite). Apply a suggestion with
  `handoff_update_task` `schedule.baseline_start` / `baseline_due` if the
  re-planned dates should become the new plan.

## 5. Impact analysis before editing

`handoff_trace_impact` is read-only. Run it before changing an approved or
heavily linked item. It has four entry modes (use exactly one):

| mode | arguments | what it simulates |
|---|---|---|
| item | `item`, plus `proposed` or `proposed_file` (a Markdown block with the item's heading) | that item's new `def_hash`/`ac_hash`. Without proposed text, "assume changed" |
| document | `doc`, plus `proposed_body` or `proposed_body_file` | every item in the document. Ids missing from the proposal count as removed |
| file | `file` | items whose `impl_refs`/`test_refs`/document `scope_paths` match the file (no hash simulation) |
| git diff | `git_diff: true` | as file mode, for every path in `git diff HEAD --name-only` |

```
handoff_trace_impact {"item": "REQ-003", "proposed": "### REQ-003 Lock the account\n\nAfter 3 failed logins ...\n"}
```

Read the output:

- `changed`: ids whose definition would change.
- `removed`: ids the proposal deletes, with the downstream references and
  task links that would become dangling (not suspect). Deleting through
  `doc_save` / `doc_update_section` unlinks those tasks automatically and
  reports them as `orphaned_tasks`; they dangle only after a direct file
  edit.
- `would_suspect.links` / `would_suspect.tasks`: exactly the suspects the
  change would create. Only already-baselined links are included.
- `rerun_candidates`: verifiers of changed items, plus changed items that
  are themselves verifiers. Re-run these after the edit. In file and git
  diff modes this is the only populated list.
- `potential`: ripple beyond one hop (depth 2 and more). Informational only,
  because suspects never propagate past one hop. Capped by `limit`.
- `tree_children_affected`: decomposition children of changed or removed
  items, with `would_orphan` for children left without any parent.

## 6. Change proposals (deltas)

Use a delta when an agent should *propose* body, link or runtime changes and
a human should decide. Nothing touches the documents until the delta is
applied.

```
handoff_trace_update {"ops": [...], "propose": true, "description": "Split REQ-003 lockout rule"}
handoff_trace_delta  {"action": "create", "ops": [...], "description": "..."}
```

- Both forms run the full validation and diff preview first. A validation
  failure leaves no delta behind. They return
  `{delta_id, ops_count, previews:[{op_index, diff}]}`.
- **Allowed ops:** `upsert_item`, `link`, `unlink`, `set`. `record`,
  `clear_suspect` and `set_layer_status` are rejected. Results and suspect
  clears are facts, not proposals. A call-level `task_id` is not kept in
  the delta — give each `link`/`unlink` op its own `task`, or `apply` fails
  with "'task' is required". `propose` cannot be combined with
  `dry_run`.
- Stored in `.handoff/trace/deltas/<delta_id>.json` together with
  `baseline_hashes`: the stored `def_hash` of each targeted item at creation
  time.

Review and resolve:

```
handoff_trace_delta {"action": "list"}
handoff_trace_delta {"action": "apply", "delta_id": "<id>", "op_indices": [0, 2],
                     "executor_kind": "human", "executor_id": "pm-sato"}
handoff_trace_delta {"action": "reject", "delta_id": "<id>", "reason": "conflicts with REQ-010"}
```

- **Staleness:** a delta is `stale` when any targeted item's current
  `def_hash` differs from its baseline, or the item no longer exists. Items
  the delta itself would create are not tracked. `list` computes the flag
  fresh. `apply` refuses a stale delta unless `force: true`, which adds a
  warning. The check uses stored hashes, so run `handoff_trace_report` first
  after direct file edits.
- **Partial apply:** `op_indices` applies only those ops, through the normal
  `trace_update` write path. The delta becomes `applied`. The remaining ops
  move into a new pending delta (`remainder_delta_id`) with renumbered
  indices.
- `apply` re-validates. A failure there is reported like any
  `trace_update` failure.
- `reject` is terminal. A rejected delta can never be applied.

## 7. Project baselines (`handoff_trace_baseline`)

A project baseline is a snapshot of the whole trace graph at a point in
time. Typically you take one at each release tag.

```
handoff_trace_baseline {"action": "create", "label": "v1.4 release"}
handoff_trace_baseline {"action": "list", "limit": 10}
handoff_trace_baseline {"action": "diff", "from": "<previous baseline_id>", "to": "current"}
```

- **`create`** first regenerates `_trace_report.json` through the
  `handoff_trace_report` path (resync included). It then writes
  `.handoff/trace/baselines/<baseline_id>.json`. Each item is stored as
  `{id, layer, def_hash, refines, verifies, last_run, coverage, approval,
  suspect}`, together with `coverage_summary`, `gap_counts` and
  `state_summary`.
  - `tag` defaults to `git tag --points-at HEAD` (the first tag, or `null`).
    Create the git tag **before** the baseline. An explicit `tag` is stored
    verbatim and not checked.
  - `commit` is not filled in automatically.
- **`list`** reads the trend index `baselines/_index.json`, newest first.
  The index is rebuilt from the baseline files if missing or corrupt.
- **`diff`** compares two baselines, or a baseline and `"current"`.
  `"current"` reads the on-disk `_trace_report.json` as-is, so run
  `handoff_trace_report` first. The output:
  - `added` / `removed`: item ids present on one side only.
  - `changed`: `{id, old_hash, new_hash}` where `def_hash` differs.
  - `regression`: `{layer, axis, old_pct, new_pct}` for each layer axis
    whose covered *ratio* dropped. Here the ratio is
    `covered / (covered + partial + uncovered + waived + na)`. Note that
    this is not the reporting formula from `verification.md`, so the numbers
    differ. A shift from `covered` to `partial` counts. An axis with no
    items on the `to` side is ignored.
  - `state_changes`: `{<state>: delta}` for states whose count moved.
  - An unknown id, or `"current"` without a report file, produces a warning
    and an empty diff, not an error.

> **Three different "baselines"**
>
> | term | what it is | where | moved by |
> |---|---|---|---|
> | link baseline | the upstream hash a `refines`/`verifies` reference (or task link: `baseline_hash`) was made against | item metadata in the layer document; task files | first writing sync or link creation; `trace_suspect` `clear` / `baseline` |
> | project baseline | a snapshot of the whole graph (coverage, states, hashes) | `.handoff/trace/baselines/` | `handoff_trace_baseline` `create` |
> | schedule baseline | a task's originally planned `baseline_start` / `baseline_due` | the task's `schedule` | `handoff_auto_schedule`, `handoff_update_task`; suggested by `baseline_suggestions` |
>
> `trace_suspect action=baseline` is about link baselines only.

## 8. Data locations and git policy

All paths are relative to `.handoff/`.

| path | contents | nature | git |
|---|---|---|---|
| `docs/_doc.<slug>.md` | layer documents: body, plus item metadata in the frontmatter (hashes, link baselines, `dev_stage`, `approval`, structured waivers) | source of truth | commit |
| `tasks/…` | tasks, including requirement links with `role` and `baseline_hash` | source of truth | commit |
| `runs/<run_id>.json` | one recorded batch of results; never modified after creation | authoritative history | commit |
| `runs/_latest.json` | newest result per item (also per campaign) | derived cache, rebuilt from `runs/` | added to `.handoff/.gitignore` automatically |
| `trace/clears/<clear_id>.json` | suspect clear audit records | audit | commit |
| `trace/approvals/<approval_id>.json` | item approval audit records | audit | commit |
| `trace/layer_status.json` | explicit `under_review` / `approved` layer records (the file is removed when empty) | state | commit |
| `trace/baselines/<baseline_id>.json` | project baseline snapshots | history | commit |
| `trace/baselines/_index.json` | coverage-trend index | derived, rebuilt if missing | optional; not ignored automatically |
| `trace/deltas/<delta_id>.json` | change proposals and their status | workflow state | commit |
| `trace/test_runs/<test_run_id>.json` | campaign definitions, checklists, evidence references | workflow state | commit |
| `reports/<report_id>.md` / `.json` | generated reports and their approval metadata | deliverables | commit |
| `templates/<report_type>.md.hbs` | report template overrides | configuration | commit |
| `docs/_trace_report.json` | full derivation report for UIs; carries an `inputs` freshness fingerprint | derived, rewritten by `handoff_trace_report` | not ignored automatically; safe to ignore or delete |
| `docs/_requirements_summary.json` | requirement summary cache | derived | not ignored automatically; safe to ignore |
| `docs/_task_ids_rebuild.json` | task-id repair marker | derived | added to `.handoff/.gitignore` automatically |

- Derived files can be deleted. They are rebuilt on the next call that needs
  them.
- Evidence paths in runs and campaigns are references only. The server does
  not copy evidence files, so commit them in the project repository.
- If `.handoff/` is kept in its own repository, apply the same rules there.

For tool signatures see `tools.md`.
