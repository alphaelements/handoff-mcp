# Running verification, approving, and reporting

How verification progress is derived, how to get results into the graph, how
approval and the done guard work, and which report to produce when. This is a
workflow guide. Full argument lists and response shapes are in `tools.md`.
Configuration keys (`done_guard`, profiles, `default_needs`) are in
`config.md`. Hashes, baselines and suspects are in `change-management.md`.
How to write the items themselves is the separate `handoff-requirements`
skill.

Example calls show the tool name followed by its JSON arguments.

## 1. The three per-item axes

Every trace item carries three independent values. None of them is derived
from another. A requirement can be `approved` while still `failing`, or
`passing` while still `draft`.

| axis | values | driven by |
|---|---|---|
| `dev_stage` | `not_started`, `in_progress`, `implemented`, `tested`, `verified` | status of tasks linked with role `implements` (plus manual `set`) |
| `state` | `passing`, `failing`, `blocked`, `not_run`, `uncovered` | recorded runs, coverage, and the states of children and verifiers |
| `approval` | `draft`, `review`, `approved` | explicit `handoff_trace_update` `set` ops, automatic reset on definition change |

### 1.1 `dev_stage`

When a task's status changes through `handoff_update_task`, each item it
links with role `implements` gets a new `dev_stage`: the minimum over all
tasks linked to that item, where `todo`/`blocked` imply `not_started`,
`in_progress` implies `in_progress`, and `review`/`done` imply
`implemented`. `skipped` tasks are ignored.

- `tested` and `verified` are manual-only. Set them with
  `handoff_trace_update` `{"op":"set","item":"REQ-003","dev_stage":"tested"}`.
  Once an item is at `tested` or `verified`, task propagation never moves it.
- Links with role `executes` never move `dev_stage`. Verification-layer items
  (right side) never receive a propagated `dev_stage`.
- `dev_stage` feeds `handoff_trace_next` (`create_task` targets
  `not_started` items; `rerun` targets verifiers of items at `implemented` or
  later) and the `dev_stage` filter of `handoff_trace_tasks`.

### 1.2 `state`

`state` is recomputed on every graph build. Recorded results come from the
newest run per item (`runs/_latest.json`).

**Verification items (right side):** the latest result. `pass` gives
`passing`, `fail` gives `failing`, `blocked` gives `blocked`. `not_run`, no
result at all, or `skipped` give `not_run`.

**Definition items (left side):** the worst of these elements. The order is
`failing` > `blocked` > `not_run` > `uncovered` > `passing`.

- each in-scope verifier's own latest result (`skipped` contributes nothing),
- the item's own latest result, if it is inline-verified (has `- method:` or
  `- test:`),
- the `state` of each in-scope refining child,
- the `state` of each requirement-tree child, for a composite,
- `uncovered`, if its horizontal or vertical coverage is `uncovered` or
  `partial`.

With no element at all, the result is `uncovered`. If the only elements were
`skipped` results, the result is `not_run`.

> A waiver counts as "not asked". A left item whose horizontal coverage is
> `waived` (`- waive-verify:`, or an empty `- needs:`), with no verifier and
> no run of its own, derives `state: passing` as long as its vertical axis
> raises nothing (it is `covered`, `waived` or `na`). A waiver exempts only
> its own axis: a waived item that is still vertically `uncovered` or
> `partial` stays `uncovered`. The waiver therefore no longer holds the
> layer back from `verified` (section 6.2).

A verification composite (a group of tests) takes the worst state of its
children and its own result.

### 1.3 `approval`

- `draft`: the default for every item.
- `review`: proposed for approval. Anyone may set it. It is **not** a done
  guard blocker.
- `approved`: signed off. Setting it stamps `approved_hash` (the current
  `def_hash`), `approved_by` (the call's `executor_id`) and `approved_at`,
  and writes an audit file under `.handoff/trace/approvals/`.

Any target value may be set directly. `draft` to `approved` is allowed. The
server does not check who sets `approved`. Make it a human action by
convention, and always pass `executor_kind: "human"` and an `executor_id`.
Without `executor_id`, `approved_by` stays empty.

**Automatic reset:** when a layer sync finds that an item's `def_hash`
changed (its title, statement, acceptance criteria or branch label was
edited), an item in `review` or `approved` drops back to `draft`.
`approved_hash` is kept as the hash at the last approval. See
`change-management.md` for which calls sync, and one exception.

## 2. Coverage

Coverage applies to left-side items in a layer that is in use. Items in a
layer that is not in use are excluded from every count.

- **Horizontal** (verification): does the item have a valid verifier?
- **Vertical** (refinement): is the item refined further down, or
  implemented by a task?

### 2.1 Horizontal classification

The first matching rule wins.

1. `covered`: the item is inline-verified, **or** a verifier references the
   whole item (`verifies: REQ-003`), **or** AC-level verifiers
   (`verifies: REQ-003#AC2`) together cover every declared acceptance
   criterion.
2. `partial`: AC-level verifiers cover some, but not all, of the item's
   acceptance criteria.
3. `waived`: the item's effective `needs` is empty (`- needs:` with no
   value).
4. If the item's paired verification layer is in use: `waived` when the
   item has `- waive-verify:`, otherwise `uncovered`.
5. Otherwise `na`.

Only verifiers in scope count. When `needs` gating applies (section 4), only
verifiers on a needed layer count.

### 2.2 Vertical classification

1. If a deeper left-side layer is in use, the item is "base covered" when it
   has a counted refining child **or** an `implements` task. If no deeper
   layer is in use, only an `implements` task counts.
2. Not base covered: `waived` when the effective `needs` is empty or the item
   has `- waive-refine:`, otherwise `uncovered`.
3. Base covered with children: `partial` if any counted child is itself
   vertically `uncovered` or `partial`, otherwise `covered`.
4. Base covered by a task only: `covered`.

An `implements` task counts once it has **left `todo`**: `in_progress`,
`review`, `blocked`, `done` and any other status make the item vertically
`covered`. If the item's only `implements` tasks are still `todo`, the item
is vertically `partial` (planned, not started). That is not `uncovered`, so
no `unrefined` gap is raised, but `partial` keeps `state` at `uncovered` and
the layer out of `verified` until work starts. One started task among
several `todo` ones is enough. A deeper refining child still covers the
item on its own, as before. Only the project-wide graph (`handoff_trace_report`,
`handoff_trace_slice`, ...) knows task statuses; a caller that supplies none
counts every task as started.

A requirement-tree composite without acceptance criteria of its own takes
both axes from the leaves below it: all covered gives `covered`, any covered
or partial gives `partial`, all uncovered gives `uncovered`.

### 2.3 Percentages and gaps

Per-layer counts are `{covered, partial, uncovered, waived, na}` on each
axis. Verification-layer items count as `na` on both axes of their own
layer. Use the same formula everywhere:

```
coverage % = covered / (total - na - waived)
```

`partial` counts in the denominator only.

| gap kind | meaning |
|---|---|
| `unverified` | horizontal `uncovered` (or a composite with no acceptance criteria or verifier of its own) |
| `unrefined` | vertical `uncovered` on a non-composite item |
| `orphan` | a left item with no `refines` while an upper left layer is in use, or a right item with no `verifies` (suppressed by `- derived:`) |
| `dangling` | a `refines`/`verifies` target that resolves to no item |
| `invalid_link` | an edge that breaks the layer rules: `refines` must point at a strictly upper left item, `verifies` must go from a right item to a left item at the same or an upper level |
| `cycle` | a `refines` cycle |
| `duplicate_id` | one `stable_id` defined in more than one document |
| `task_unlinked` | a task doc-links a layer document but has no item-level link into it |

> **`covered` is not `verified`.** Under a profile with implicit acceptance
> (`minimal`, `bugfix`), every acceptance criterion is materialized as an
> implicit verification item (`REQ-003#AC1`). The requirement is therefore
> horizontally `covered` the moment its acceptance block exists. Whether
> those checks pass shows only in `state`. Record results against the
> implicit ids. Likewise, vertical `covered` only means a task has been
> started. Read `state`, not coverage, to judge progress.

## 3. Linking tasks to items

Link through `handoff_update_task`. `requirement_ids` **replaces** the set
on an existing task: ids that are newly present are added, and ids that are
now absent are removed.

A fresh project has `[settings] require_estimate_hours = true`: a leaf task
moving to `in_progress` / `review` / `done` (or created there) needs
`schedule.estimate_hours` in the same call, and `trace_tasks(mode: "apply")`
needs `estimate_hours`.

```
handoff_update_task
{"task": {"id": "t12", "requirement_ids": ["REQ-003", "AT-REQ-003-1"],
          "requirement_roles": {"AT-REQ-003-1": "executes"}}}
```

- Role inference when `requirement_roles` omits an id: an item on a
  verification layer gets `executes`, anything else gets `implements`.
- Adding a link records its baseline hash (`change-management.md`).
- `handoff_get_task` (and `handoff_task_checklist` in view mode) returns a
  `trace` field for any task with a requirement link:
  `{layers:[{layer, role, count}], blockers:{not_run, failing, blocked,
  reverify, suspect, approval_draft}}`. It is `null` for tasks without one.
- `handoff_list_tasks` accepts `layer` and `role` filters. A task matches
  when a single requirement link satisfies both.
- `handoff_trace_update` `link`/`unlink` ops do the same per link, in bulk.
- `handoff_trace_tasks` creates the missing tasks in bulk. It creates an
  `implements` task for each left item that has none, and an `executes` task
  for each right item that has none and is not already passing. Preview
  first, then apply:

```
handoff_trace_tasks {"select": {"layers": ["acceptance"]}, "mode": "preview"}
handoff_trace_tasks {"select": {"layers": ["acceptance"]}, "mode": "apply", "estimate_hours": 0.5}
```

## 4. `needs` and `assignee`

Both attributes have real behavior.

**`- needs: <layer>, <layer>`** controls which layers count toward a left
item's coverage. The same list gates both axes: verifiers for horizontal,
refining children for vertical.

| item's `needs` | effect |
|---|---|
| absent | the project default profile's `default_needs` for the item's layer applies; with no profile, or no entry for that layer, the item is not gated |
| `- needs:` (empty) | no coverage required; an otherwise uncovered axis reports `waived` |
| a list | only verifiers or children on those layers count |

- A verifier on a layer outside the item's needs is excluded from coverage
  and reported by lint `unwanted_coverage` (info).
- If the list names only a verification layer, refining children no longer
  count, so vertical coverage then depends on an `implements` task. Include
  the refining layer as well if children should count.
- Unknown layer ids are dropped with a warning.

**`- assignee: <key>`** names who runs a check. The key is validated against
the `[assignees.<key>]` roster in `config.toml`. An unknown key is stored
anyway, with a sync warning.

- `handoff_trace_next {"assignee": "alice"}` restricts the action list to
  that person's items.
- The `manual_pending` next-action (rank 3, next to `rerun`) lists items that
  have an assignee, `method` set to `manual`, `visual` or `review`, and
  `state: not_run`.
- `handoff_trace_test_run` `scope.assignee` builds a campaign for one person.

## 5. Recording results

Valid results: `pass`, `fail`, `blocked`, `not_run`, `skipped`. `skipped` is
excluded from `state` aggregation. An explicit `not_run` counts as not run.

| situation | tool |
|---|---|
| A few results, on their own | `handoff_trace_record` |
| Results together with other edits (body, links, approval) in one atomic batch | `handoff_trace_update` `record` op |
| A cargo or JUnit test report | `handoff_trace_ingest` |

```
handoff_trace_record
{"results": [{"item": "AT-REQ-003-1", "result": "pass",
              "evidence": ["screenshots/lock.png"], "note": "checked on staging"}],
 "executor_kind": "human", "executor_id": "alice", "task_id": "t12"}
```

- One call writes one new file, `.handoff/runs/<run_id>.json`. Run files are
  never modified afterwards. The derived cache `runs/_latest.json` (newest
  result per item) is refreshed and is gitignored automatically.
- Each result stores the item's current `def_hash` and `body_hash`. This is
  what later turns a pass into a `result` suspect when the item changes. An
  item added by editing the `.md` file directly is recorded without a hash
  (and can never become a result suspect) until a sync has seen it — run
  `handoff_trace_report` first after hand edits.
- An unknown `item` is recorded anyway, with a warning.
- `commit` defaults to `git rev-parse --short HEAD`.
- `handoff_trace_record` resyncs only the documents that own the recorded
  items. All `record` ops in one `handoff_trace_update` call merge into one
  run file. That op has no `test_run_id`. Use `handoff_trace_record` or
  `handoff_trace_ingest` to attribute results to a campaign.
- Neither tool refreshes `_trace_report.json`. Call `handoff_trace_report`
  afterwards if a reader depends on it.
- `handoff_trace_history {"item": "AT-REQ-003-1"}` lists every recorded
  result, newest first.

### 5.1 Ingesting test reports

```
handoff_trace_ingest {"format": "junit_xml", "output_file": "target/nextest/ci/junit.xml", "dry_run": true}
```

- **JUnit XML (recommended):** run `cargo nextest run` with a
  `[profile.<name>.junit]` section in `.config/nextest.toml`. This needs no
  unstable flags.
- **cargo JSON:** there is no stable `cargo test --format json`. Use
  `cargo +nightly test -- -Z unstable-options --format json`, or on stable
  `RUSTC_BOOTSTRAP=1 cargo test -- -Z unstable-options --format json`.

Matching against items:

- An item with `- test:` attributes is matched in two stages: (1) exact
  test-name match, then (2) a `::`-boundary suffix match. For example
  `test: lock::lock_after_5` matches `tests::e2e::lock::lock_after_5`. The
  item is recorded only if **every** declared `test` value matched. Any value
  without a match goes to `missing_refs`, and the item is not recorded in
  this call, so a partial run never overwrites a fuller one.
- An item without `- test:` uses (3) the naming convention: `REQ-003` maps
  to the test-name prefix `test_req_003`.
- Results per item: any `fail` gives `fail`, all `skipped` gives `skipped`,
  otherwise `pass`. JUnit `<failure>`/`<error>` count as fail and
  `<skipped>` counts as skipped. libtest `ignored` counts as skipped.
- `unmatched_tests_count` counts tests that matched no item.
- Run `dry_run: true` first and check `matched` and `missing_refs`.

Ingest does not resync directly edited documents before recording. After
editing layer files by hand, call `handoff_trace_report` first so the
recorded hashes are current.

## 6. Approval

### 6.1 Item approval

```
handoff_trace_update
{"ops": [{"op": "set", "item": "REQ-003", "approval": "review"}]}

handoff_trace_update
{"ops": [{"op": "set", "item": "REQ-003", "approval": "approved"}],
 "executor_kind": "human", "executor_id": "pm-sato"}
```

`approved_hash` is taken from the stored `def_hash`. After editing a layer
file directly, run `handoff_trace_report` before approving.

### 6.2 Layer lifecycle

Each in-use layer has a status in `handoff_trace_report` `layer_statuses`.

| status | how it arises |
|---|---|
| `not_started` | the layer has no items |
| `in_progress` | any item is not `passing`, or a V-model gap is attributed to the layer |
| `verified` | every item is `passing` and the layer has no gap |
| `under_review`, `approved` | explicit record, honoured only while the layer stays `verified` |

```
handoff_trace_update
{"ops": [{"op": "set_layer_status", "layer": "acceptance", "status": "approved"}],
 "executor_kind": "human", "executor_id": "qa-lead"}
```

- `status` is `under_review`, `approved` or `reset`. The first two are
  rejected unless the layer's derived status is `verified` at that moment.
  The check runs after every other op in the same call. `reset` drops the
  record.
- **Automatic demotion:** when a layer stops being `verified` (a failure, a
  new gap, a new item that is not passing), the next rewrite of
  `_trace_report.json` deletes its record. The layer then shows its derived
  status. Fixing the problem later does not re-approve it. Set the status
  again.
- `project_status` is `complete` when every in-use layer is `approved`.
  Otherwise it is the lowest status, in the order `not_started` <
  `in_progress` < `under_review` < `verified`. An in-use layer with no items
  stays `not_started` and keeps the project from completing.
- Records live in `.handoff/trace/layer_status.json`.
- `set_layer_status` is not allowed in change proposals.
- Requirement-tree findings never affect layer status.

Campaign approval is a separate lifecycle (section 7).

## 7. Verification campaigns (`handoff_trace_test_run`)

Use a campaign for checks a test report cannot produce: manual, visual or
review checks, release acceptance, and sign-off with evidence. A campaign is
a frozen list of target items, a checklist, and a lifecycle.

```
handoff_trace_test_run
{"action": "create", "label": "v1.4 acceptance",
 "scope": {"layers": ["acceptance"], "assignee": "alice"}}
```

- `scope.layers` / `scope.assignee` select items by effective layer and
  assignee. Without `layers`, every layer item is a target, so restrict it.
  `scope.kinds` (the `handoff_trace_next` vocabulary, e.g.
  `["rerun", "manual_pending"]`) selects what the next-action derivation
  would surface instead.
- `scope.items` (stable_ids) names items directly and narrows the other keys;
  an id outside the matched set is skipped with a warning. Those four are the
  **only** scope keys: an unknown key is rejected. A scope that matches
  nothing is refused rather than creating an empty campaign, and a campaign
  made by mistake can be removed with `{"action": "delete", "test_run_id":
  "<id>"}` (not once it is `approved`).
- Under `minimal`/`bugfix`, implicit `X#ACn` items cannot carry `assignee`,
  so `scope.layers: ["acceptance"]` takes every criterion in the project. To
  verify just one item's criteria, record them directly with
  `handoff_trace_record` (no campaign), or keep campaigns release-wide.
- `auto_checklist` (default `true`) creates one `pending` checklist row per
  target: `{item_id, acceptance_text, result, evidence, note, verified_by,
  verified_at}`. With `auto_checklist: false` the checklist starts empty and a
  row is created the first time a target is checked; `completed` still
  requires every target to be checked.
- Stored in `.handoff/trace/test_runs/<test_run_id>.json`. The file is
  updated in place by the campaign actions.

Checklist actions:

```
handoff_trace_test_run
{"action": "record_check", "test_run_id": "<id>", "item_id": "AT-REQ-003-1",
 "result": "pass", "verified_by": "alice",
 "evidence": [{"path": "evidence/lock.png", "type": "screenshot", "caption": "locked after 5 tries"}]}
```

- `result`: `pending`, `pass`, `fail`, `blocked` or `waived`.
  `verified_by` defaults to the calling agent.
- `evidence` (array of `{path, type, caption}`) is appended. `path` must be
  non-empty without `..`; `type` defaults to `file`. `add_evidence` appends
  one entry without changing the verdict.
- The first non-pending check moves `draft` to `in_progress`. Both actions
  are rejected on `completed`/`approved` campaigns and for an `item_id` not
  in the checklist.

Lifecycle with `set_status`:

| from | to | condition |
|---|---|---|
| `draft` | `in_progress` | none |
| `in_progress` | `completed` | at least one target, none `pending` (a target with no row yet counts as pending) |
| `completed` | `in_progress` | reopen |
| `completed` | `approved` | `approved_by` required; no `fail`/`blocked` row (unless `accept_failures: true`); terminal |

`set_status` to `draft` is always refused: `draft` is only the state a
campaign is created in.

`completed` means "every row has been executed", so a campaign with `fail`
or `blocked` rows can be completed. `approved` means "the results were
reviewed and accepted" and is terminal, so it is refused while any row is
`fail` or `blocked`. Resolve the rows first: reopen (`completed` to
`in_progress`), re-run the check and record `pass`, or mark the row
`waived`. To sign off a failed result on purpose (for example an inspection
certificate that records a failed inspection), pass `accept_failures: true`
together with `approved_by` on the `approved` call.

`progress` returns run-based counts (results recorded with this
`test_run_id`) and, separately, `checklist_progress`.

> **`record_check` does not write a run and does not change `state`.** It
> only updates the campaign file. That is enough for the verification and
> inspection reports built from the campaign. It does nothing for item
> `state`, layer status, the done guard, or the completion report. Those
> read runs, and only `handoff_trace_record` / `handoff_trace_ingest` (with
> `test_run_id`) write runs.

**Recommended pattern for manual checks that must count:** for each check,
record the run **and** the checklist row.

```
handoff_trace_record
{"results": [{"item": "AT-REQ-003-1", "result": "pass", "evidence": ["evidence/lock.png"]}],
 "executor_kind": "human", "executor_id": "alice", "test_run_id": "<id>"}

handoff_trace_test_run
{"action": "record_check", "test_run_id": "<id>", "item_id": "AT-REQ-003-1",
 "result": "pass", "verified_by": "alice",
 "evidence": [{"path": "evidence/lock.png", "type": "screenshot"}]}
```

Map `pass`/`fail`/`blocked` one to one. A `waived` check has no run
equivalent. Handle a waiver on the item itself (a body `waive-verify`, or a
structured waiver, section 9) instead of recording a run.

## 8. Done guard

`[trace] done_guard` applies when `handoff_update_task` moves a task to
`review` or `done`. Under `block`, creating a task directly in one of those
statuses is checked too.

| value | behavior |
|---|---|
| `warn` (default) | the change applies and the blocker summary is appended to the response |
| `block` | the call is rejected unless `force: true` |
| `off` | no check |

A task without requirement links is never checked. The blocker tally is the
same one `handoff_get_task` shows under `trace.blockers`. Per requirement
link:

| blocker | counted when |
|---|---|
| `not_run`, `failing`, `blocked` | `implements` link: each in-scope verifier of the item (and the item's own run, if inline-verified) in that state. `executes` link: the linked item's own state |
| `reverify` | an item counted in the row above is `passing`, but its own result or one of its `verifies` links is suspect |
| `suspect` | this task's own link to the item is a `task` suspect |
| `approval_draft` | the linked item itself is `draft`, for either role. `review` and `approved` are not blockers |

- An `implements` link to an unintegrated composite is judged by the
  verified descendants below it.
- An item with **no verifier at all** produces no `not_run`/`failing`
  blocker. Check `handoff_trace_next {"task_id": "t12"}` for missing
  verification before closing.
- Because new items start as `draft`, every freshly linked item is an
  `approval_draft` blocker. Move linked items to at least `review` before
  closing the task. Under `block`, this is required.
- `handoff_bulk_update_tasks` applies the same guard per update (`block`
  rejects the update into `errors[]` unless the call has `force: true`;
  `warn` returns a `warnings[]` entry). Only an editor or UI that writes
  task files directly bypasses it.

```
handoff_update_task {"task": {"id": "t12", "status": "done"}, "force": true}
```

Use `force` only with a stated reason. Record the reason in the task notes.

## 9. Bug and defect flow

1. A verification fails. Record it (`result: "fail"`). The item's `state`
   becomes `failing`, and its layer demotes on the next report.
2. Open a bug task, linked to the failing item:

```
handoff_update_task
{"task": {"title": "Lockout counter resets on reload", "priority": "high",
          "labels": ["bug", "bug:fix"], "requirement_ids": ["AT-REQ-003-1"]}}
```

   - The label `bug` marks a bug. The disposition label is one of `bug:fix`,
     `bug:defer`, `bug:waive` or `bug:not_a_bug`. An unknown disposition
     appears as a report warning.
   - Severity is the task `priority` (`low` / `medium` / `high` — task
     priorities are not `P0`–`P3`; those belong to items).
   - Linking to the failing verification item infers role `executes`, and
     the requirement's `dev_stage` is not touched. Under
     `done_guard = "block"` the bug cannot be closed until a passing result
     is recorded **and** the linked item is out of `draft` approval (under the
     default `warn` the blockers are only reported). If you link the
     requirement instead (`implements`), the bug task's status takes part in
     that requirement's `dev_stage`.
3. Fix it, re-run, and record `pass`. If the definition changed, clear or
   re-verify the resulting suspects (`change-management.md`).
4. For `bug:waive`, store a structured waiver record:
   `{"op": "set", "item": "AT-REQ-003-1", "waive_reason": "...", "waive_approved_by": "product-owner"}`.
   It appears as `items[].waive` in the trace report and always adds a
   `waiver_added` warning. It does **not** change coverage or `state`. The
   item stays `failing` until a new result is recorded.

The defect report (`report_type: "defect"`) lists every `bug`-labelled task
with status, severity, disposition and linked items. Counts: `open` (not
`done`/`skipped`), `closed`, and `open_without_disposition`. It also lists
every item whose `state` is exactly `failing` (`blocked` is not included),
with its bug tasks and `has_open_bug`. `failing_items_without_open_bug`
counts failures nobody is working on.

## 10. Reports (`handoff_report`)

| report_type | use it for | scope / precondition |
|---|---|---|
| `verification` | test or acceptance results with evidence | `layers`, `items`, `statuses`, or `campaign` (rows come from the checklist); otherwise latest runs |
| `inspection` | an inspection certificate | `campaign` required and must be `approved`; no other filters |
| `defect` | a defect register | `period`/`from`/`to` (bug creation date), `assignee`, `layers`, `items` |
| `completion` | project close-out | refused unless `project_status` is `complete`; only `label` |
| `weekly` | weekly status | `period` (ISO week or date range); default current week |
| `monthly` | monthly roll-up with metric trends | `period` (month, week or range); default current month |
| `milestone` | plan vs actual for one milestone | `milestone` required |
| `effort` | hours vs estimates | `period`/`from`/`to`, `assignee` |

```
handoff_report {"action": "generate", "report_type": "verification",
                "scope": {"campaign": "<test_run_id>", "label": "v1.4 ATR"}}
handoff_report {"action": "submit",  "report_id": "<id>"}
handoff_report {"action": "approve", "report_id": "<id>", "reviewer": "qa-lead"}
handoff_report {"action": "reject",  "report_id": "<id>", "reviewer": "qa-lead", "comment": "evidence missing for AT-7"}
```

- `generate` creates a `draft` (version = 1 + existing reports of the same
  type and scope). Flow: `draft` → `submitted` → `approved`, or →
  `revision_requested` (reason required) → `submitted` again.
- Stored as `.handoff/reports/<report_id>.md` + `.json` (metadata). `data`
  keys overlay the collected data.
- Every type except `effort` runs the trace report internally (resync,
  `_trace_report.json` rewrite, layer demotion).
- Override a template with `.handoff/templates/<report_type>.md.hbs`. A
  template that fails to compile is an error, not a silent fallback.

## 11. Release verification checklist

1. `handoff_trace_report {}`: resync and refresh. Read `warnings`.
2. `handoff_trace_lint {"fail_on": "warning"}`: aim for `exit_code: 0`, or
   document the accepted findings.
3. `handoff_trace_suspect {"action": "list"}`: no suspects and empty
   `reverify`. Re-run tests or clear suspects with a reason.
4. `handoff_trace_next {"limit": 50}`: empty, or only kinds you have
   consciously deferred.
5. Campaign: `create`, then record runs **and** checks for every target,
   `set_status` to `completed`, review the failures, then `set_status` to
   `approved` with `approved_by`.
6. Items: move them to `review`, then a human sets them to `approved`.
7. Layers: once `layer_statuses` shows `verified`, set `set_layer_status`
   to `approved` for each layer, until `project_status` is `complete`.
8. Tag the release in git, then
   `handoff_trace_baseline {"action": "create", "label": "v1.4"}` (the tag
   is picked up from `HEAD`). Diff it against the previous baseline and check
   `regression` (`change-management.md`).
9. Reports: `verification` (campaign), `inspection`, `defect`, `completion`.
   Then `submit` and `approve` each.

For tool signatures see `tools.md`.
