# Trace tool reference

Signatures and semantics for the `handoff_trace_*` tool family and `handoff_report`. This file covers what each tool takes, returns and writes. It does not walk through workflows.

- Workflow-level guides: `verification.md` (recording results, campaigns, approval, layer status, the done guard, the bug flow, reports) and `change-management.md` (hashes, baselines, suspects, impact, deltas, sync rules, data locations).
- Configuration keys (`[trace]`, `[trace.lint.rules]`, `[[trace.lint.require]]`, profiles, custom layers): `config.md`.
- Lint rule catalogue: `lint-rules.md`.
- How to write layer documents, items, acceptance criteria and the requirement tree: the separate `handoff-requirements` skill.

Conventions used below:

- Every tool also accepts `project_dir` (default: the current working directory). It is left out of the signatures.
- Every `handoff_trace_*` tool **rejects an unknown argument** (top level, or inside an object such as `scope`/`filter`/`select`) with an error that names it, instead of ignoring it. A misspelled key never silently widens a call.
- File paths (`output_file`, `proposed_file`, `proposed_body_file`) are relative to `project_dir`; an absolute path is used as given. (`trace_matrix`'s `output_file` is stricter: relative only, no `..`, not inside `.handoff/`.)
- `name?` means optional, `="x"` is the default, and `a | b` means "exactly one of".
- Every response is a JSON string. Most responses have a `warnings[]` array. Read it, because many non-fatal conditions (unknown ids, truncation, waivers) show up only there.
- "Item" means a trace item, identified by its `stable_id` (for example `REQ-003` or `AT-REQ-003-1`).
- **Writes** lists every file a call can change. "Resync" means the server re-parses a layer document whose Markdown body was edited directly on disk since its last sync, then saves the refreshed item metadata (and `_requirements_summary.json`). See `change-management.md`.

## Contents

| Tool | Purpose | Read-only? |
|---|---|---|
| [`handoff_trace_report`](#handoff_trace_report) | Full derivation report: coverage, gaps, layer status, items | No (resync, `_trace_report.json`, layer-status demotion) |
| [`handoff_trace_slice`](#handoff_trace_slice) | Neighbourhood of one task or item | No (resync only) |
| [`handoff_trace_tree`](#handoff_trace_tree) | Nested requirement-tree view with roll-ups | Yes |
| [`handoff_trace_matrix`](#handoff_trace_matrix) | CSV/Markdown traceability matrix | Yes (unless `output_file`) |
| [`handoff_trace_next`](#handoff_trace_next) | Ranked "what to do next" list | Yes |
| [`handoff_trace_impact`](#handoff_trace_impact) | Impact of a proposed change | Yes |
| [`handoff_trace_lint`](#handoff_trace_lint) | Rule-based lint; quality prompt templates | Yes |
| [`handoff_trace_suspect`](#handoff_trace_suspect) | List, clear and baseline suspects | `list` and `baseline` dry-run only |
| [`handoff_trace_propose`](#handoff_trace_propose) | Find covering items and draft a new one | Yes |
| [`handoff_trace_scaffold`](#handoff_trace_scaffold) | Generate verification items from acceptance criteria | `preview` only |
| [`handoff_trace_tasks`](#handoff_trace_tasks) | Generate missing implements/executes tasks | `preview` only |
| [`handoff_trace_update`](#handoff_trace_update) | Bulk mutation: items, links, runtime fields, results, suspects, layer status | No (`dry_run` is read-only) |
| [`handoff_trace_record`](#handoff_trace_record) | Record one batch of results | No |
| [`handoff_trace_ingest`](#handoff_trace_ingest) | Parse cargo/JUnit output and record results | No (`dry_run` is read-only) |
| [`handoff_trace_history`](#handoff_trace_history) | Result history of one item | Yes |
| [`handoff_trace_test_run`](#handoff_trace_test_run) | Test runs and verification campaigns | `list`/`progress`/`get` only |
| [`handoff_trace_baseline`](#handoff_trace_baseline) | Snapshots, trend index, diffs | `list`/`diff` only |
| [`handoff_trace_delta`](#handoff_trace_delta) | Pending change proposals | `list` only |
| [`handoff_report`](#handoff_report) | Markdown reports and their approval workflow | `list`/`get` only |

---

## Read and analysis tools

### handoff_trace_report

Builds one derivation graph from every layer document, the task-to-item links and the latest recorded results, and returns coverage, gaps and layer status.

**Writes:**

- Resyncs any directly edited layer document.
- Repairs drift in item `task_ids` when the task links changed since the last rebuild.
- Rewrites `.handoff/docs/_trace_report.json`, but only when its content changed and only when no `layers` override was given.
- Removes `under_review`/`approved` records from `.handoff/trace/layer_status.json` for layers that are no longer `verified` (that is, it demotes them).

```
handoff_trace_report(layers?: [layer], gap_kinds?: [kind], limit?=50, include_items?=false)
-> {trace_layers:{in_use, source}, coverage:{<layer>:{total, horizontal, vertical, state}},
    gaps:[{kind, item, layer, detail}], gap_counts:{<kind>: n},
    layer_statuses:{<layer>: status}, project_status, tree, totals, warnings, items?}
```

- `layers`: overrides the in-use layer set **for this response only**. Without it, the server uses `[trace] layers`, then the default profile's layers, then the layers that actually have items. A call that passes `layers` does not write `_trace_report.json`, but it still resyncs.
- `gap_kinds`: filters `gaps[]` only. `gap_counts` always counts every kind.
  - V-model kinds: `unverified`, `unrefined`, `orphan`, `dangling`, `invalid_link`, `cycle`, `duplicate_id`, `task_unlinked`.
  - Requirement-tree kinds: `parent_dangling`, `parent_cross_layer`, `tree_cycle`, `self_parent`, `empty_group`, `kind_conflict`, `condition_missing`, `condition_duplicate`, `condition_on_non_branch`, `attr_unknown_parent`, `decision_arity`, `empty_branch`.
  - Tree kinds never affect `layer_statuses`.
- `limit`: truncates `gaps[]` after filtering. A truncation is reported in `warnings`.
- `include_items`: adds `items[]` to the response, in the same shape the persisted file always carries.

**Key output fields:**

- `coverage.<layer>`: `horizontal` (verification) and `vertical` (refinement) are each `{covered, partial, uncovered, waived, na}`; `state` is `{passing, failing, blocked, not_run, uncovered}`. Coverage % = `covered / (total - na - waived)`; `partial` counts in the denominator only.
- `layer_statuses.<layer>`: one of `not_started` (no items), `in_progress`, `verified` (every item passing and no gap attributed to the layer), `under_review` or `approved`. The last two are explicit records set via `trace_update` `set_layer_status`, honoured only while the layer stays `verified`.
- `project_status`: `complete` when every layer is `approved`. Otherwise it is the lowest layer status.
- `tree`: `{roots:{<layer>:[id]}, max_depth, composite_count, decision_count, select_count, leaf_count, dag_shared_count}`.
- `totals`: `{nodes, leaves, composites, decisions, selects, max_depth, dag_shared, state, progress}`, with shared DAG nodes counted once.
- `items[]` fields: `id`, `layer`, `side`, `title`, `doc`, `seq`, `sub_item_index`, `category`, `state`, `refines`, `verifies`, `tasks:[{id,role}]`, `priority`, `dev_stage`, `impl_refs`, `test_refs`, `last_run?` (`{run_id, executed_at, result, stale}`), `profile` (governing profile names), `def_hash`, `coverage:{horizontal,vertical}`, `suspect:[{kind, upstream?, task?, link_type, baseline_hash, current_hash}]`, `reverify`, `approval`, `acceptance:[{id,label,kind}]`, `implicit_of?`, `derived?`, `from?`, `waivers:[{axis,reason}]` (from the body), `waive?:{reason, approved_by}` (structured waiver record).
  - Requirement tree: `parents`, `parent_declared`, `children`, `kind` (`leaf|composite|decision|select`), `conditions?`, `order`, and `subtree:{depth, parents, children, leaves, state, dev_stage, approval, excluded, suspect, progress, tasks, all_tasks}`.
- An empty-looking result (no layers, no items) carries structured `DIAG-T0xx` diagnostics in `warnings` that explain why.

The persisted `_trace_report.json` always contains the full item list and every gap (unfiltered). It also carries `schema_version` (currently 3), an `inputs` freshness fingerprint, `layer_defs`, `profile`, `suspect_counts` and `tasks`. Readers can compare the fingerprint to detect a stale file. Other tools (`trace_record`, `trace_ingest`, document saves) do **not** refresh it. Call `handoff_trace_report` afterwards.

```json
{"gap_kinds": ["unverified", "dangling"], "limit": 20}
```

### handoff_trace_slice

Returns the trace neighbourhood around one task or item. Use it in place of a full report to save context.

**Writes:** resyncs directly edited layer documents (the resync warnings appear in `warnings`). It does not write `_trace_report.json`.

```
handoff_trace_slice(task_id | item, direction?: "up"|"down"|"both"="both", depth?,
                    expand?: [id], max_items?=30)
-> {items:[{id, layer, side, title, state, refines, verifies, tasks:[{id,role}], profile,
            coverage:{horizontal,vertical}, suspect, reverify, approval,
            parents, children, via, statement?}], truncated, warnings}
```

- `task_id`: the starting set is every item that task links to. A task with no links returns `items: []`. An unknown task or item is an **error**, never an empty result.
- `direction`: `up` follows `refines` toward parents, `verifies` toward the verified item, and declared tree `parent`s; `down` follows refining children, verifiers and tree children; `both` is the union of the two one-way walks (not a walk that turns around).
- `depth`: caps each one-way walk. The default is unlimited, and a cycle always stops the walk.
- `expand`: items that also get `statement`, re-extracted from the current body.
- `max_items`: when the reachable set is larger, the farthest items are dropped first and `truncated: true` is set. Dangling ids never use up a slot.
- `via`: how the item was first reached. One of `start`, `refines`, `verifies` or `parent`.

```json
{"task_id": "t12", "direction": "up", "expand": ["REQ-003"]}
```

### handoff_trace_tree

Returns a nested view of the requirement tree (the same-layer decomposition DAG) with per-node roll-ups. It uses the same numbers as `trace_report`.

**Writes:** nothing. Directly edited documents are re-parsed in memory only.

```
handoff_trace_tree(root? | scope?, layer?, depth?, filter?: {state?, priority?, suspect?,
                   gaps_only?, kind?}, fields?: ["own.<key>"|"subtree.<key>"])
-> {roots:[node], totals:{nodes, leaves, composites, decisions, selects, max_depth,
    dag_shared, truncated_subtrees}, warnings}
node = {id, title, kind, layer, doc, seq, order, priority, condition?, conditions?,
        own:{state, dev_stage, approval, coverage, suspect:[kind], tasks, acceptance_criteria},
        subtree:{depth, parents, children, leaves, state, dev_stage, approval, excluded,
                 suspect, progress, tasks, all_tasks},
        parents, has_children, children:[node]|null}
```

- `root`: one item's tree (an unknown id is an error). `scope`: an id prefix such as `"REQ-AUTH-"`. Matching items without a matching parent become roots. The two are mutually exclusive. With neither, the tool returns every layer's roots, in-use layers first.
- `layer`: limits the roots to one layer (an unknown layer is an error).
- `depth`: `0` returns roots only. Nodes below the limit have `has_children: true` and `children: null`.
- A node with several parents is expanded only at its first occurrence. Later occurrences have `children: null` and are counted in `truncated_subtrees`.
- `filter`: conditions are ANDed and matching nodes keep their ancestors; roll-ups stay unfiltered. `state` (`passing|failing|blocked|not_run|uncovered`), `priority` (`P0..P3`), `suspect` (boolean), `kind` (`leaf|composite|decision|select`), `gaps_only` (nodes with a defect: own state uncovered or failing, a suspect, a tree finding, or a wiring gap — `orphan`, `dangling`, `invalid_link`, `cycle`, `duplicate_id`; `not_run`/`blocked` are work in progress and `unrefined`/`task_unlinked` are backlog, so they do not count — use `trace_lint` for every gap kind).
- `fields`: trims `own`/`subtree`. These are always returned: `id`, `kind`, `parents`, `has_children`, `children`, `condition` and `conditions`. `title`/`layer`/`doc`/`seq`/`order`/`priority` appear only when `fields` is omitted.
- Sibling order already follows each parent's `order@` values, so read `children[]` in order.

### handoff_trace_matrix

Exports the trace graph as a flat CSV or Markdown table.

**Writes:** nothing, except `output_file` when you give one.

```
handoff_trace_matrix(format: "markdown"|"csv", shape?: "tree"|"edges"="tree", root_layer?,
                     layers?: [layer], include_tasks?=true, output_file?)
-> {format, shape, root_layer?, columns, rows, content?, output_file?, warnings}
```

- `shape="tree"`:
  - One row per item of `root_layer`. The default root layer is the shallowest in-use left-side layer.
  - Columns: one per in-use layer (left side top-down, then right side top-down), then `tasks`, `state` (the root's aggregate) and `suspect` (a count).
  - When the root layer has `parent` edges, the table becomes a depth-first tree. A leading `item` column indents two spaces per depth. A multi-parent id is starred (`REQ-A2*`) and expanded only once. A `parents` column is added.
- `shape="edges"`: one row per resolved link, `{from, to, link_type, from_layer, to_layer, state, suspect}`. Decomposition edges appear with `link_type: parent` (child to parent), and a trailing `condition` column is added when any branch label exists. Dangling links never appear here; use lint to find them.
- `layers`: restricts or reorders the columns. Canonical order wins over the order you give. Unknown or unused layers are dropped with a warning.
- Cell formatting: several ids join with `; ` in CSV and `<br>` in Markdown. CSV follows RFC 4180 (every cell quoted, LF line endings, no BOM).
- `output_file`: a path relative to the project directory. Absolute paths and paths containing `..` are rejected. When it is given, `content` is omitted.

### handoff_trace_next

Ranks "what to do next" across the trace graph and suggests a follow-up call for each action.

**Writes:** nothing.

```
handoff_trace_next(task_id?, layers?: [layer], assignee?, kinds?: [kind], limit?=10)
-> {actions:[{rank, kind, item?, task?, priority?, reason, suggest:{tool, arguments}}],
    truncated, warnings}
```

| Rank | Kind | Fires when | `suggest.tool` |
|---|---|---|---|
| 1 | `fix_failing` | A verification item's state is failing or blocked | `handoff_trace_slice` (then fix, then record) |
| 2 | `review_suspect` | An item has a suspect upstream link | `handoff_trace_impact` (then fix the body, or clear the suspect) |
| 3 | `rerun` | A verifier needs reverification or has never run, and its verified target is `implemented` or later (for an inline item, its own `dev_stage`) | `handoff_trace_ingest` (`{format:"junit_xml"}`) |
| 3 | `manual_pending` | An item with an assignee whose `method` is `manual`, `visual` or `review` is still `not_run` | `handoff_trace_record` (`results:[{item, result:"not_run"}]`; replace `not_run` with the real outcome) |
| 4 | `write_verification` | A left-side item **with acceptance criteria** has uncovered or partial horizontal coverage (an item without criteria has nothing to scaffold and gets no action; `trace_lint` still reports it as `unverified`) | `handoff_trace_scaffold` into the pair layer's document (`{items:[id], target_doc}`), or `handoff_doc_save` to create that layer's first document |
| 5 | `refine` | A left-side item's vertical coverage is uncovered or partial, and a deeper left-side layer exists to refine into | `handoff_trace_update` (dry-run `upsert_item` adding a child that `refines` the item in the next deeper layer's document), or `handoff_doc_save` to create that layer's first document |
| 6 | `create_task` | A `not_started` left-side item has no implementing task and is not already vertically covered through its refining children | `handoff_trace_tasks` (`mode:"preview"`) |
| 7 | `fix_link` | A structural gap: `dangling`, `invalid_link`, `cycle`, `duplicate_id` or `orphan` | `handoff_trace_slice` (then fix the body) |
| 8 | `baseline` | A refines/verifies reference has no baseline yet | `handoff_trace_suspect` (`{action:"baseline", dry_run:false, scope:{doc}}`, scoped to the item's document) |
| 9 | `relink_candidate` | A verifier (or an implementing task) links straight to an item that a newer, one-level-deeper left-side item now refines | `handoff_trace_update` (dry-run `upsert_item` with `verifies` rewritten; only to a deeper item the verifier's layer level may legally verify), or `handoff_update_task` for a task (keeps the task's other links) |

**Ordering:**

- Actions sort by rank first. All `rerun` actions come before all `manual_pending` actions, even though both have rank 3.
- Within a rank, actions sort by item priority (P0, P1, P2, P3, then unknown), then by layer level (upper layers first), then by `stable_id` in natural order.

**Parameters:**

- `task_id`: limits candidates to that task's linked items.
- `layers` / `assignee`: applied before ranking, so out-of-scope candidates never push in-scope ones past `limit`. `assignee` must match the item's assignee exactly.
- `kinds`: an empty array is an error, and so is an unknown kind.
- `limit`: truncates the final list. `truncated` tells you whether more candidates existed.

**The `suggest` field:** `suggest.arguments` is valid against the named tool's input schema, so it can be called as returned. Two things to check before you do:

- Placeholders: `refine` proposes a generated id and a generic title (`Refinement of <id>`), and `manual_pending` records `not_run`. Edit them to the real values.
- Writes: `baseline` writes (`dry_run:false`); the `trace_update` suggestions are dry-run previews, so re-send them without `dry_run` to apply.

**Requirement tree:** `write_verification`, `refine` and `create_task` fire only for leaves, and for composite/decision/select nodes that have their own acceptance criteria.

### handoff_trace_impact

Analyses the impact of a proposed change before you make it.

**Writes:** nothing, including `_trace_report.json`.

```
handoff_trace_impact(item [+ proposed | proposed_file]
                   | doc + (proposed_body | proposed_body_file)
                   | file
                   | git_diff: true,
                   limit?=50)
-> {changed:[id], removed:[{id, downstream_refs:[{child,type}], tasks:[id]}],
    would_suspect:{links:[{child, upstream, type}], tasks:[{task, item}]},
    rerun_candidates:[id], potential:[{id, depth}], tree_children_affected:[...],
    truncated, warnings}
```

Exactly one of the four entry modes is allowed:

| Mode | Input | What it simulates |
|---|---|---|
| 1. item | `item`, plus `proposed` (a Markdown block containing that item's heading) or `proposed_file` | Re-parses the block to get the new definition and acceptance-criteria hashes. With neither proposal given, the item is assumed changed (it is compared against every baseline). |
| 2. doc | `doc` (a layer document; others are rejected), plus `proposed_body` or `proposed_body_file` (the full new body) | Every item in both versions whose hash would change. Items the new body no longer defines go to `removed`. |
| 3. file | `file` (a path) | Every item whose `impl_refs`, `test_refs` or `scope_paths` match the file is treated as an implementation change. No hash simulation, so only `rerun_candidates` is filled. |
| 4. git_diff | `git_diff: true` | Same as `file`, for every path in `git diff HEAD --name-only`. |

**Output fields:**

- `would_suspect`: only downstream links and task links that already have a baseline. Unbaselined links are never guessed.
- `removed`: the direct references and task links that would become **dangling**. They do not become suspect. These are also echoed into `warnings`.
- `rerun_candidates`: verifiers of every changed item, plus changed items that are themselves verifiers.
- `potential`: ripple effects at depth 2 and beyond. This is informational only, because suspect status never propagates more than one hop. Capped by `limit`.
- `tree_children_affected`: `[{parent, change:"changed"|"removed", children:[id], would_orphan?:[id]}]`. Decomposition children are not hashed links and never become suspect; these are the children to re-read. `would_orphan` lists the children left with no surviving parent.

```json
{"item": "REQ-003", "proposed": "### REQ-003 Lock after 5 failures\n\nThe system shall lock ..."}
```

### handoff_trace_lint

Runs rule-based lint over the whole trace graph. It can also return LLM prompt templates for quality review.

**Writes:** nothing for `lint` and `quality_prompt` (directly edited documents are re-parsed in memory only). `nfr_check` merges its result into `_trace_report.json` (see below).

```
handoff_trace_lint(action?: "lint"|"quality_prompt"|"nfr_check"="lint",
                   rules?: [id], fail_on?: "error"|"warning"="error",
                   format?: "json"|"text"="json", limit?,      # action=lint
                   items?: [id], aspects?: [aspect])           # action=quality_prompt
                   items?: [id], review?: {category: status}   # action=nfr_check
```

**`action="lint"`**

```
-> {findings:[{rule, severity, item?, task?, doc?, message}], counts:{error, warning, info},
    exit_code, warnings, text?}
```

- Evaluates the built-in rules together with the project's `[[trace.lint.require]]` policy rules. The full rule list, default severities and policy syntax are in `lint-rules.md`. Per-rule severity overrides (`[trace.lint.rules]`) are in `config.md`.
- `rules`: limits evaluation to these rule or policy ids. An unknown id is an error.
- `fail_on`: the severity that sets `exit_code` to 1.
- `limit`: truncates `findings`. `counts` and `exit_code` still reflect every match.
- `format="text"`: adds a rendered `text` field. The CLI prints this field.
- Findings are sorted by severity (highest first), then rule id, then item in natural order.

**`action="quality_prompt"`**

```
-> {prompts:[{item_id, title, text, aspects:[{name, prompt_template, context}]}], warnings}
```

- The server never calls an LLM itself. It returns templates for you to fill in and send.
- `items`: defaults to every requirement-layer item. An empty array is an error.
- `aspects`: any of `singular`, `verifiable`, `unambiguous`, `complete`, `feasible`, `traceable`, `necessary`, `implementation_free`, `consistent` (aligned with ISO/IEC/IEEE 29148), or `completeness_set`. Unknown names are rejected. The default is the nine per-item aspects (not `completeness_set`).
- `completeness_set` is set-level and exclusive (pass it alone). It returns one prompt `{scope:"set", items_included, item_ids, text, aspects}` covering every selected item. See `lint-rules.md`.
- `prompt_template` contains a literal `{text}` placeholder. Substitute the item text. `title` and `text` currently hold the same value (the item's title line).

**`action="nfr_check"`**

```
-> {categories:[{name, status, items:[id], prompt_template, context}], unclassified:[id], warnings}
```

- The ISO/IEC 25010 NFR checklist. `categories` always has the eight entries `performance_efficiency`, `compatibility`, `usability`, `reliability`, `security`, `maintainability`, `portability`, `functional_suitability`, in that order.
- Classification: an `NFR-` item is classified from its own id segments (`NFR-SECURITY-001`, `NFR-PERF-002`) and, if the id names nothing, from keywords in its text. An `NFR-` item that matches nothing is listed in `unclassified`. Every other item counts as `functional_suitability`.
- `items`: defaults to every requirement-layer item. An empty array is an error; an unknown id is skipped with a warning.
- `status`: `applicable` when the category has items, else `not_reviewed`. Record your own judgement of a template with `review: {"portability": "not_applicable"}` (`reviewed`, `applicable`, `not_applicable`, `not_reviewed`). Recorded decisions persist across calls; `not_reviewed` clears one.
- **Writes:** the result (without the templates) goes into `.handoff/docs/_trace_report.json` as `nfr_coverage` (`{categories, unclassified, scope, reviews}`). The report is rebuilt first when it does not exist, and later `trace_report` rebuilds keep `nfr_coverage`.

### handoff_trace_suspect

Lists, clears and baselines suspects: links, task links or results whose upstream definition changed after they were baselined.

**Writes:**

- `list`: nothing.
- `baseline` with `dry_run: true`: nothing.
- `clear` and `baseline` with `dry_run: false`: resync first, then write link baselines into layer-document metadata, task baselines into task files and result carry-forwards into `runs/`. `clear` also writes an audit file to `.handoff/trace/clears/<id>.json`.

```
handoff_trace_suspect(action?: "list"|"clear"|"baseline"="list", ...)
```

**`list`:**

```
(item?, task_id?, kinds?: ["link"|"task"|"result"], layers?, limit?=50)
-> {suspects:[{kind, item, upstream?, task?, link_type?, baseline_hash, current_hash}],
    counts:{link, task, result}, unbaselined:{links, tasks}, reverify:[id], truncated}
```

- `link`: a child's `refines`/`verifies` reference whose baseline no longer matches the upstream definition hash. For an `X#ACn` reference, the hash of that acceptance criterion is compared.
- `task`: a task's link whose baseline no longer matches the item.
- `result`: a verifier's latest `pass` was recorded against an older definition.
- `unbaselined`: links with no baseline yet. These are never suspects.
- `reverify`: passing verifiers whose own result, or one of whose `verifies` links, is suspect. Their `state` is **not** changed.

**`clear`:**

```
(targets: [target], reason, evidence?: {run_id?, commit?, note?}, executor_kind?="ai", executor_id?)
-> {cleared:{links, tasks, results}, clear_id, baseline_suggestions, warnings}
```

| Target shape | Clears |
|---|---|
| `{item, upstream}` | That one link |
| `{item}` | All of that item's suspect links |
| `{upstream}` | Every link pointing at that upstream |
| `{task_id, item?}` | That task's link(s) |
| `{layer}` | Every suspect in that layer |
| `{result: item}` | That item's result suspect |

- Link and task clears move the baseline to the current hash.
- A result clear records one new run that carries the last result forward against the current hashes.
- `baseline_suggestions` is advisory; nothing is written. It has one entry per cleared task that has schedule dates: `{task_id, current_start, current_due, baseline_start?, baseline_due?, action}`. `action` is one of:
  - `set_recommended`: the task has no schedule baseline yet.
  - `update_recommended`: the baseline differs from the schedule.
  - `manual_baseline_detected`: the task is pinned and has a differing baseline. Do not overwrite it.

  To act on a suggestion, call `handoff_update_task(schedule)`.

**`baseline`:**

```
(dry_run?=true, scope?: {doc} | {layer})
-> {baselined:{links, tasks, results:0}, dry_run, warnings}
```

- A migration helper that gives unbaselined links and task links a baseline.
- It never touches a link that is already suspect.

### handoff_trace_propose

Suggests existing items that may already cover a task, and drafts a ready-to-review template for a new item.

**Writes:** nothing. Apply the draft yourself after review, with `handoff_doc_save`, `handoff_doc_update_section` or a `trace_update` `upsert_item` op.

```
handoff_trace_propose(task_id | title, notes?, prefix?, parent?, limit?=5)
-> {candidates:[{id, title, layer, score}],
    proposal:{profile, doc, markdown, next_ids} | null, warnings}
```

- `task_id`: uses the task's title, notes and `scope_paths`. `title` (plus optional `notes`) is for a task that does not exist yet.
- `candidates`: lexical and semantic similarity between your query and each item's title. Document bodies are not read.
- `proposal`:
  - The layer set comes from `[trace] layers`, then the default profile, then a standard-shaped fallback. Any fallback is reported in `warnings`.
  - The new item goes in the deepest left-side layer of that set.
  - Minimal-style profile (implicit acceptance): one item with an inline acceptance-criteria block.
  - Otherwise: the item plus a paired verification item.
- IDs: `<prefix>-<highest existing number for that prefix + 1>`, zero-padded to 3 digits. The prefix is the layer's first prefix unless you pass `prefix` (for example `CART`). `prefix` must be one of the target layer's registered prefixes (built-in defaults plus `[trace.id_prefixes]`); otherwise the call fails and lists the allowed ones. It is ignored, with a warning, when `parent` is given.
- Every fill-in slot in the template (statement, acceptance bullet, branch label) is the language-neutral marker `TBD`; replace it. The paired verification item is titled `<title> [<verification layer id>]`.
- `doc`: the layer document whose `scope_paths` overlap the task's. When no such document exists, `doc` is a suggested new slug that is never created (flagged in `warnings`).
- `parent`: proposes a same-layer decomposition child instead:
  - The id is `<parent>-<next number>`, and the template carries `- parent: <parent>`.
  - If the parent is a decision or select, the template carries a `- condition: TBD` line (replace it with a label unique among the parent's children), and a warning says so.
  - The call fails if the parent does not exist or is not on a left-side layer.
- `proposal` is `null` when no left-side layer or layer pair can be resolved.

---

## Generators

### handoff_trace_scaffold

Generates one verification item per acceptance-criteria bullet of a left-side item.

**Writes:** `mode="apply"` appends the items to `target_doc`'s body in one document save, which also runs a layer sync. `preview` writes nothing.

```
handoff_trace_scaffold(items: [id] | doc, target_doc, mode?: "preview"|"apply"="preview", limit?=20)
-> {target_doc, mode, applied, generated:[{id, from, title}], skipped:[{ac, existing}], warnings}
```

**Generated ID:**

- The format is `<target layer's default prefix>-<source id>-<AC number>`. For example, AC1 of `REQ-003` scaffolded into an `acceptance` document becomes `AT-REQ-003-1`.
- If that id is already taken anywhere in the project, a single letter is appended (`AT-REQ-003-1a`, `...1b`, and so on). A warning is returned if all 26 letters are taken.

**Generated body:**

- Attributes: `verifies: <source id>`, `from: <source id>#<AC label>` and `method: manual`.
- A Given/When/Then criterion is split into steps (`手順`, from the Given/When clauses) and expected result (`期待結果`, from the Then clause, which is also the heading title).
- An EARS or plain-text criterion gets a fill-in placeholder for steps, its full text as the expected result, and its first 40 characters as the title.

**Idempotency via `from:`:** a criterion that already has an item anywhere in the project with `from: <source id>#<AC label>` is skipped. Skipped criteria are listed in `skipped` and do not count against `limit`, so re-running scaffold is safe.

**Notes:**

- `items` and `doc` are mutually exclusive. `doc` selects every item in that document that has an acceptance-criteria block.
- Requirement tree: the sources are leaves, plus composite and decision items that have their own acceptance criteria. A composite without criteria gets a warning that points at its leaves.
- If a criterion's implicit (auto-materialized) verification item already has a recorded run, a warning says that the run is **not** carried over to the new item.

### handoff_trace_tasks

Creates one task for each item that is still missing the task role it needs.

**Writes:** `mode="apply"` creates tasks. Scanning never writes; documents are re-parsed in memory only.

```
handoff_trace_tasks(items: [id] | select: {layers?, gap_kinds?, dev_stage?}, parent_id?,
                    estimate_hours?, default_milestone?, mode?: "preview"|"apply"="preview", limit?=20)
-> {mode, created|planned:[{task_id?, item, role, kind?, title}], skipped:[{item, role, existing}], warnings}
```

**Targets:**

- A left-side item with no task holding an `implements` link to it.
- A right-side item with no `executes` task, whose latest result is not `pass`.
- Items that already have the role are listed in `skipped` (the call is idempotent).

**Filters:**

- `items` and `select` are mutually exclusive. Omitting both scans every item.
- `select.gap_kinds`: `unverified`, `unrefined`, `orphan`, `dangling`, `invalid_link`, `cycle`, `duplicate_id`, `task_unlinked`.
- `select.dev_stage`: `not_started`, `in_progress`, `implemented`, `tested` or `verified`.
- Unknown ids in `items` are reported in `warnings`.

**Each ordinary task:** title `"<stable_id> <item title>"`, a requirement link with role `implements` or `executes`, label `layer:<effective layer id>`, `scope_paths` copied from the owning document, status `todo`. Done criteria are **not** copied. (VRT tasks differ: see below.)

**Options:**

- `parent_id`: makes every new task a child of that task.
- `estimate_hours`: applied to every task the call creates. It is **required** for `apply` when `[settings] require_estimate_hours` is on.
- Creation goes through the same validation as `handoff_update_task`.
- Requirement tree: an `implements` task is generated only for leaves, and for composite/decision items with their own acceptance criteria. Other composites are skipped with a warning, even when named in `items`.
- `default_milestone`: a milestone id set as `schedule.milestone` on every task the call generates (ordinary and VRT) whose item has no effective milestone of its own. The item's `- milestone:` attribute, else its document's milestone, always wins. Omitted: generated tasks get no milestone. A non-string or empty value is an error.

**VRT tasks (GUI verification):** an item carrying a `- mock:` attribute yields a second, separate task next to its ordinary one:

- Title `"[VRT] <stable_id> <item title>"`, labels `vrt` and `gui-verification`, no requirement link. The entry in `created`/`planned` has `kind: "vrt"` and `role: "vrt"`.
- Three fixed `done_criteria` (all unchecked): golden screenshot captured, VRT (visual regression test) PASS, AI visual verification PASS. These are the only generated tasks that carry done criteria.
- Idempotent on its own full title: an existing `[VRT] ...` task is listed in `skipped`, and the ordinary task is unaffected.
- An item without `- mock:` generates only the ordinary task.
- The `milestone` report's `gui` section reads these tasks back (see [`handoff_report`](#handoff_report)).

---

## Mutation

### handoff_trace_update

The bulk-mutation entry point for layer items, task links, runtime fields, results, suspect clears and layer review status.

```
handoff_trace_update(ops: [op], task_id?, dry_run?=false, propose?=false, description?,
                     executor_kind?="ai", executor_id?, commit?)
-> {applied:[{op_index, op, result}], failed?:{op_index, error}, warnings,
    suspect_introduced?, orphaned_children:[id], dry_run?}
-> (propose=true) {delta_id, ops_count, previews:[{op_index, diff}], warnings, propose:true}
```

**Validation:**

- All ops are validated **before anything is written**, in input order. This covers shapes, enums and every referenced document, item and task.
- An op may reference an item that an earlier `upsert_item` in the same call creates.
- If validation fails, nothing is written and `failed.op_index` is the op's position in the input array.

**Write order:**

- Writes run in a fixed category order regardless of input order: `upsert_item`, then `link`/`unlink`, then `set`, then `record`, then `clear_suspect`, then `set_layer_status`.
- A rare write-time failure (I/O or lock) stops before the next category. `applied` then shows what already landed. There is no cross-file rollback.
- If a touched document changes on disk between validation and write, all ops are re-validated and retried, up to 3 times.

**Call-level options:**

- `task_id`: the default task for `link`/`unlink` ops that omit `task`, and the run's task for `record` ops. With `propose: true` it is copied into each `link`/`unlink` op that has no `task` of its own before the delta is saved, so the stored delta applies later without a call-level `task_id`.
- `commit`: passed through to `record` ops.
- `dry_run`: nothing is written. `upsert_item` results carry `result.diff`, a unified-diff hunk of the item's block (the whole block may show as removed and re-added even when one line changed). Other ops echo their resolved arguments.
- `propose`: instead of writing, stores the validated ops and their previews as a new pending delta (see [`handoff_trace_delta`](#handoff_trace_delta)).
  - It is mutually exclusive with `dry_run`.
  - Only `upsert_item`, `link`, `unlink` and `set` are allowed. `record`, `clear_suspect` and `set_layer_status` are rejected.
  - `description` is copied to the delta.

**Response fields:**

- `suspect_introduced`: present when an `upsert_item` changed an item's definition hash.
- `orphaned_children`: decomposition children left with no parent. Normally empty.

#### Op: `upsert_item`

```
{op:"upsert_item", doc, id, title?, statement?, acceptance?: [{label, text}], attrs?: {...}, after?}
```

**Where it writes:**

- Rewrites one item in a **layer document's** body. A non-layer document is rejected.
- For an existing id, only the parts you give are replaced. `attrs` is merged key by key, so the supported attributes you do not mention survive. This includes `needs` and `assignee`: they are kept when you do not mention them, and `attrs` can set them. An `attrs` key the op does not understand is ignored and reported in `warnings` (`unknown attrs key '<key>' was ignored`), so a typo such as `priorty` is visible. **`acceptance` is not merged: it replaces the item's whole criteria list** (a deliberate policy: the op states the complete list, and a label-wise merge could never delete a criterion). Every existing criterion missing from the new list is removed, together with the implicit verification items tied to it. The response says so: `warnings` gets `acceptance_dropped: ops[n] <id>: ... <labels> ... removed`, naming each verifier of `<id>#<label>` that is left dangling and its latest recorded result. Re-send every criterion you keep, and dry-run first. A malformed `acceptance` entry (not an object with string `label` and `text`) fails validation.
- A new id is inserted after `after`, or at the end of the document when `after` is omitted. An `after` id that does not resolve to an item in the document is an error.
- A new id's prefix must match the document's layer configuration.

**`attrs` keys:** `layer`, `refines`, `verifies`, `priority`, `method`, `test`, `rationale`, `derived`, `waive-verify`, `waive-refine`, `needs` (comma-separated layer ids or an array; an empty array means "requires nothing"; `""`/`null` clears), `assignee` (a single-line roster key; `""`/`null` clears), plus the requirement-tree keys:
  - `parent`: one or more same-layer ids, as a comma-separated string or an array. `""` clears it.
  - `order` and `order@<parent>`: a non-negative integer sibling order, optionally per parent.
  - `kind`: `leaf|group|decision|select`. `""` clears it.
  - `condition` and `condition@<parent>`: a one-line branch label, optionally per parent. `""` clears it.
  - The `<parent>` in `order@`/`condition@` must be in the merged parent list.

**Tree errors** are reported as `failed.errors:[{code, item, message, path?}]` with `code` one of `dotted_id` (ids like `DSP-1.2.3`), `duplicate_id` (id owned by another document), `self_parent`, `parent_dangling`, `parent_cross_layer`, `tree_cycle` (`path` shows the closed loop, including loops through earlier ops of the same call).

**Waivers:** setting or changing `derived`, `waive-verify` or `waive-refine` (including on a new item) always adds `waiver_added: <id> <axis> <reason>` to `warnings`. A waiver is never added silently.

**Batching:** several `upsert_item` ops in one call are synced together. A *newly added* reference to a same-call upstream item gets that item's new hash as its baseline; an existing link whose upstream changes in the same call still turns suspect. Each document is written once.

#### Ops: `link` / `unlink`

```
{op:"link"|"unlink", item, task?, role?: "implements"|"executes"}
```

- Adds or removes a task-to-item requirement link, using the same path as `handoff_update_task(requirement_ids=...)`.
- When `role` is omitted, it is inferred from the item's category.
- Several link ops for the same task are merged into one write of the task file.

#### Op: `set`

```
{op:"set", item, dev_stage?, approval?, impl_refs?, priority?, test_refs?,
 waive_reason?, waive_approved_by?}
```

Writes an item's runtime fields directly. At least one field is required.

| Field | Values | Layer-document items | Non-layer items |
|---|---|---|---|
| `dev_stage` | `not_started`, `in_progress`, `implemented`, `tested`, `verified` | allowed | allowed |
| `approval` | `draft`, `review`, `approved` | allowed | allowed |
| `impl_refs` | non-empty array of `{path, lines?, label?}` replaces the stored list. A malformed entry (a bare string, a missing or empty `path`, a non-string `lines`/`label`) fails validation and changes nothing. `[]` is a no-op with a warning; `null` clears the list | allowed | allowed |
| `priority` | `P0`–`P3` | **refused**: the body owns it, so edit the body or use `upsert_item` | allowed |
| `test_refs` | same shape and rules as `impl_refs` | **refused**: the body owns it (use the `test` attribute) | allowed |
| `waive_reason` + `waive_approved_by` | both non-empty strings, or both `null` to clear | allowed | allowed |

- `approval: "approved"`:
  - Stores `approved_hash` (the current definition hash), `approved_by` (the call's `executor_id`) and `approved_at`.
  - Writes an audit file to `.handoff/trace/approvals/`.
- `approval: "review"` and `"draft"` change only the approval field.
- The structured waiver (`waive_reason` + `waive_approved_by`):
  - It is a runtime record that survives re-syncs and is shown as `waive` in report items.
  - It does **not** change coverage. The body's `waive-verify`/`waive-refine` attributes drive coverage.
  - Recording one adds `waiver_added: <id> waive <reason>` to `warnings`.
  - Giving only one of the two fields is an error.

#### Op: `record`

```
{op:"record", item, result: "pass"|"fail"|"blocked"|"not_run"|"skipped", note?, evidence?: [string]}
```

- Every `record` op in the call is merged into **one** run file, via `handoff_trace_record`.
- The item is not existence-checked. An unknown id is recorded with a warning.

#### Op: `clear_suspect`

```
{op:"clear_suspect", reason, item? | upstream? | task_id? | layer? | result?}
```

- Uses the same target shapes as `trace_suspect` `clear`, flattened onto the op. For example, `{result:"AT-1"}` clears AT-1's result suspect.
- Each op makes one `trace_suspect` clear call and writes one audit file.

#### Op: `set_layer_status`

```
{op:"set_layer_status", layer, status: "under_review"|"approved"|"reset"}
```

- The explicit layer review step.
- `under_review` and `approved` require the layer's derived status to be `verified`. This is checked after all other ops in the call have been applied.
- `reset` removes the explicit record.
- It is applied last and stored in `.handoff/trace/layer_status.json`. `_trace_report.json` is then refreshed.
- A layer that later stops being `verified` is demoted by the next `trace_report`.
- Not allowed with `propose`.

**Bug flow:** a `fail` result demotes the layer automatically. Open a bug with `handoff_update_task(labels:["bug","bug:<fix|defer|waive|not_a_bug>"], requirement_ids:[...])`. For a `waive` disposition, record the structured waiver with `set`. See `verification.md`.

```json
{"ops": [
  {"op": "upsert_item", "doc": "auth-requirements", "id": "REQ-007", "title": "Session timeout",
   "statement": "The system shall end idle sessions after 15 minutes.",
   "acceptance": [{"label": "AC1", "text": "Given an idle session, when 15 minutes pass, then the user is signed out"}],
   "attrs": {"priority": "P1", "parent": "REQ-002"}, "after": "REQ-006"},
  {"op": "link", "item": "REQ-007", "role": "implements"},
  {"op": "set", "item": "REQ-007", "dev_stage": "in_progress"}
 ], "task_id": "t42", "dry_run": true}
```

---

## Recording results

### handoff_trace_record

Records one execution batch (one CI run, or one manual pass) as a single run file.

**Writes:**

- A new `.handoff/runs/<run_id>.json`.
- The refreshed `runs/_latest.json` cache.
- A resync of the layer documents that own the recorded items, and `_requirements_summary.json`.
- It does **not** refresh `_trace_report.json`; call `handoff_trace_report` afterwards.

```
handoff_trace_record(results: [{item, result, note?, evidence?: [string]}],
                     executor_kind?: "ai"|"human"="ai", executor_id?, commit?, task_id?, test_run_id?)
-> {run_id, recorded, warnings}
```

- `result`: `pass`, `fail`, `blocked`, `not_run` or `skipped`.
- Each result's body hash is filled in from the item's current hash. You never supply it. An unresolvable item is still recorded, with a warning.
- `commit`: defaults to `git rev-parse --short HEAD`, or `""` on failure.
- `test_run_id`: attributes the batch to a test run or campaign, so it counts in `trace_test_run` `progress`. The id is not checked for existence.
- `evidence` here is a list of plain strings. **Structured visual-regression (VRT) evidence is not a `trace_record` field**: it is an evidence entry of a verification campaign, recorded with `handoff_trace_test_run` `record_check` / `add_evidence` (below). Record the verdict itself with `trace_record` (same `test_run_id`) if it should count in the trace graph.
- For cargo or JUnit output, use `handoff_trace_ingest` instead.

```json
{"results": [{"item": "AT-REQ-003-1", "result": "pass", "evidence": ["docs/evidence/lock.png"]}],
 "executor_kind": "human", "executor_id": "alice"}
```

### handoff_trace_ingest

Parses a test run's output and records the matched results.

**Writes:**

- One aggregated run for layer items (same run files as `trace_record`, plus `_requirements_summary.json`). Unlike `trace_record`, ingest does **not** re-sync directly edited documents first — run `handoff_trace_report` after hand edits.
- `test_refs` label updates for items outside layer documents.
- `dry_run` writes nothing.

```
handoff_trace_ingest(format: "cargo_json"|"junit_xml", output | output_file, commit?, task_id?,
                     test_run_id?, executor_kind?="ai", executor_id?, dry_run?=false)
-> {run_id?, recorded, matched:[{item, result, tests:[name]}], missing_refs:[{item, test}],
    unmatched_tests_count, warnings, dry_run}
```

**Formats:**

- `junit_xml` is recommended. For example, `cargo nextest run` with a `[profile.<name>.junit]` section produces it without unstable flags.
- `cargo_json` is libtest's JSON-lines output. It needs `cargo +nightly test -- -Z unstable-options --format json`, or on stable `RUSTC_BOOTSTRAP=1 cargo test -- -Z unstable-options --format json`.
- `output` takes priority over `output_file`. `output_file` is read relative to `project_dir` (an absolute path is used as given). `trace_impact`'s `proposed_file` / `proposed_body_file` and `trace_matrix`'s `output_file` use the same base.

**Matching:** the three rules are tried in priority order:

1. An exact match against one of the item's `- test:` values.
2. A `::`-boundary suffix match. For example, `test: lock::lock_after_5` matches `tests::e2e::lock::lock_after_5`.
3. Only for items with **no** `test:` attribute: a test whose last `::` segment starts with a name derived from the item's `stable_id`. The derived name is the lowercased `stable_id` with each run of non-alphanumerics replaced by `_`, prefixed with `test_` (`REQ-001` -> `test_req_001`). The match must end at a word boundary: the test name either ends there or continues with a non-alphanumeric character. So `REQ-1` matches `test_req_1` and `test_req_1_locks`, but not `test_req_10` or `test_req_1a`. A child id is still a prefix of its descendants (`REQ-1` also matches `test_req_1_2`), so prefer explicit `test:` values where ids nest.

**Aggregation and coverage:**

- Per item, results aggregate across all matched tests: any fail wins, then all-skipped, otherwise pass.
- An item that declares `test:` values is recorded **only if every declared value matched**. Otherwise it is listed in `missing_refs` and left untouched, so a partial run never overwrites a fuller result.

### handoff_trace_history

Returns every recorded result for one item, newest first.

**Writes:** nothing.

```
handoff_trace_history(item, limit?=20)
-> {items:[{run_id, executed_at, executor:{kind, id?}, result, note, evidence, commit}]}
```

### handoff_trace_test_run

Manages test runs (a fixed target set whose progress is computed from runs) and **verification campaigns** (the same object, plus a checklist and an approval lifecycle).

**Writes:** `create`, `record_check`, `add_evidence`, `set_status` and `delete` write `.handoff/trace/test_runs/<test_run_id>.json` only. `list`, `progress` and `get` are read-only.

```
handoff_trace_test_run(action?: "create"|"list"|"progress"|"get"|"record_check"|"add_evidence"|"set_status"|"delete"="create", ...)
```

| Action | Arguments | Returns |
|---|---|---|
| `create` | `scope?: {layers?, kinds?, assignee?, items?}`, `label?`, `auto_checklist?=true` | `{test_run_id, target_items, total_target_count, campaign_status, checklist_count, warnings}` |
| `list` | `limit?=20` | `{test_runs:[{test_run_id, created_at, label, total_target_count, campaign_status, progress}], truncated}` |
| `progress` | `test_run_id` | `{test_run_id, total, executed, pass, fail, blocked, skipped, not_run, progress_pct, remaining:[id], campaign_status, checklist_progress, warnings}` |
| `get` | `test_run_id` | The full record, including `checklist` |
| `record_check` | `test_run_id`, `item_id`, `result`, `note?`, `verified_by?`, `evidence?: [entry]` | The campaign summary (below) |
| `add_evidence` | `test_run_id`, `item_id`, `evidence: entry` | The campaign summary |
| `set_status` | `test_run_id`, `status`, `approved_by?`, `accept_failures?` | The campaign summary |
| `delete` | `test_run_id` | `{deleted}`. Removes the campaign definition; an `approved` campaign cannot be deleted. Runs already attributed to it stay. |

The campaign summary is `{test_run_id, campaign_status, progress:{total, checked, pass, fail, blocked, waived, pending}, completed_at, approved_by, approved_at}`.

**`create`:**

- With `scope.kinds` (from `trace_next`'s kind vocabulary), the targets are the items `trace_next` would surface for those kinds. This needs a full graph build.
- Without `kinds`, the targets are every layer item that matches `scope.layers`/`scope.assignee`. No graph build is needed.
- `scope.items` names stable_ids directly. It narrows the other keys (an id outside the matched set is skipped with a warning).
- A scope that matches no item is refused: no empty campaign is created. Unknown scope keys are rejected too.
- The run definition is never overwritten.
- By default, one pending checklist row is created per target item: `{item_id, acceptance_text, result, evidence, note, verified_by, verified_at}`.
- With `auto_checklist: false` the checklist starts empty. `record_check` / `add_evidence` create a row the first time a target item is used, and `completed` requires every target to have been checked, so the campaign still completes. An id that is not a target is rejected.

**Two separate result channels.** This distinction matters:

| | `trace_record(test_run_id=…)` / `trace_ingest(test_run_id=…)` | `trace_test_run(action="record_check")` |
|---|---|---|
| Writes a run file in `runs/` | **yes** | **no** |
| Changes the item's trace `state`, coverage and layer status | **yes** | **no** |
| Counted by `progress` (`pass`/`fail`/`not_run`…) | **yes** | no |
| Updates the campaign checklist (`checklist_progress`, `campaign_status`) | no | **yes** |

- `record_check` changes only the campaign file's checklist row.
- To make a manual verdict count in the trace graph as well, also call `handoff_trace_record` (or a `trace_update` `record` op) with the same `test_run_id`.
- `progress` treats a target with no result recorded against this test run as `not_run`. It is recomputed on every call.

**`record_check` fields:** `result` is `pending`, `pass`, `fail`, `blocked` or `waived` (setting `pending` clears `verified_by`/`verified_at`); `verified_by` defaults to the calling agent's id; `note` replaces the row's note; `evidence` is appended. The first non-pending check moves `draft` to `in_progress`.

**Evidence entry:** `{path, type?="file", caption?, vrt?}`. `path` must be non-empty and must not contain `..`; `type` must be non-empty. `record_check` takes an array of entries; `add_evidence` takes a single entry and leaves the verdict unchanged.

**VRT evidence (`vrt`):** an optional object that attaches visual-regression detail to an entry. Every field is optional, paths are project-relative (non-empty, no `..`), and `similarity_pct` / `threshold` are percentages in `0..=100` (out of range is an error).

```
evidence: {
  path: "docs/evidence/login-diff.png", type: "vrt-diff",
  vrt: {
    baseline_path: "path/to/golden.png",   # the golden screenshot
    actual_path: "path/to/actual.png",
    diff_path: "path/to/diff.png",
    threshold: 99,                          # minimum similarity to pass, percent
    similarity_pct: 99.5,                   # measured similarity, percent
    viewport: "1280x720",
    tool: "pixelmatch"
  }
}
```

- `type` is a free string; the conventions are `golden`, `vrt-diff` and `vrt-actual`.
- Verdict: `similarity_pct >= threshold` is a VRT pass, below it a VRT fail. When either number is missing no verdict is claimed.
- `verification` and `inspection` reports render each entry's `vrt` as one line, for example `VRT pass: 99.5% similarity (threshold 99%); viewport 1280x720; tool pixelmatch; [baseline](..) [actual](..) [diff](..)`.
- `threshold` is a percentage like `similarity_pct` (`99`, not `0.01`).

**`set_status` transitions:**

| From | Allowed to | Condition |
|---|---|---|
| `draft` | `in_progress` | |
| `in_progress` | `completed` | The checklist is non-empty and has no pending row. `fail`/`blocked` rows are allowed (completed means every row was executed). Sets `completed_at`. |
| `completed` | `in_progress` | This reopens the campaign and clears `completed_at`. |
| `completed` | `approved` | `approved_by` is required, and no row may be `fail` or `blocked` unless `accept_failures: true` is passed (otherwise reopen and re-run the row, or mark it `waived`). Sets `approved_at`. |
| `approved` | none | Terminal. |

`record_check` and `add_evidence` are rejected on `completed` and `approved` campaigns. To change a completed campaign, reopen it first. An approved campaign is what an `inspection` report certifies (see [`handoff_report`](#handoff_report)).

```json
{"action": "record_check", "test_run_id": "20261018-101500-123-482913", "item_id": "AT-REQ-003-1",
 "result": "pass", "evidence": [{"path": "docs/evidence/lock.png", "type": "screenshot", "caption": "Locked after 5 tries"}]}
```

---

## History and change control

### handoff_trace_baseline

Takes baseline snapshots of the trace graph, keeps a coverage-trend index and diffs snapshots.

**Writes:** `create` writes:

- A regenerated `_trace_report.json` (the same path `trace_report` uses, including the resync). This is a no-op when nothing changed.
- A new `.handoff/trace/baselines/<baseline_id>.json`.
- An appended entry in `.handoff/trace/baselines/_index.json`.

`list` and `diff` are read-only.

```
handoff_trace_baseline(action?: "create"|"list"|"diff"="create", ...)
```

| Action | Arguments | Returns |
|---|---|---|
| `create` | `tag?`, `label?`, `commit?`, `executor_kind?="ai"`, `executor_id?` | `{baseline_id, tag, items_count, coverage_summary, warnings}` |
| `list` | `limit?=20` | `{baselines:[{baseline_id, created_at, tag, label, total_items, coverage_summary}], truncated}` (newest first) |
| `diff` | `from`, `to` (each a `baseline_id` or `"current"`) | `{added, removed, changed:[{id, old_hash, new_hash}], regression:[{layer, axis, old_pct, new_pct}], state_changes:{<state>: delta}, warnings}` |

**Git tags:** the tool only **reads** tags; it never creates one.

- When `tag` is omitted, it records the first tag from `git tag --points-at HEAD`, or `null` if there is none.
- An explicit `tag` is stored verbatim, with no check against git.
- `commit` is **not** auto-resolved.

**Snapshot contents:**

- Per item: `{id, layer, def_hash, refines, verifies, last_run, coverage, approval, suspect}`.
- Top level: `coverage_summary`, `gap_counts`, `state_summary`.
- The index is rebuilt from the snapshot files if it is missing or corrupt.

**`diff`:**

- `"current"` reads the on-disk `_trace_report.json` as-is and never regenerates it. Run `trace_report` first if it may be stale.
- `changed`: items whose definition hash differs between the two sides.
- `regression`: a layer and axis whose coverage rate dropped. The rate is `covered / (total - na - waived)`, the same as the trace report's, so waiving an item or marking it n/a is never a regression. Coverage moving from covered to partial counts. An axis with no item left to cover on the `to` side is never reported.
- `state_changes`: only states whose count changed.
- An unresolvable `from`/`to` is not an error. It is reported in `warnings` and all fields are empty.

### handoff_trace_delta

Manages change proposals ("deltas"): bundles of ops held for review before they touch any document.

**Writes:** `create`, `apply` and `reject` write `.handoff/trace/deltas/<delta_id>.json`. `apply` also performs the real `trace_update` writes. `list` is read-only.

```
handoff_trace_delta(action?: "create"|"list"|"apply"|"reject"="list", ...)
```

| Action | Arguments | Returns |
|---|---|---|
| `create` | `ops` (non-empty), `task_id?` (default `task` for `link`/`unlink` ops that omit it; copied into each such op before saving), `description?`, `executor_kind?`, `executor_id?` | `{delta_id, ops_count, previews:[{op_index, diff}], warnings}` |
| `list` | `status?: "pending"|"applied"|"rejected"|"all"="pending"`, `limit?=20` | `{deltas:[{delta_id, created_at, status, description, ops_count, stale}], truncated}` |
| `apply` | `delta_id`, `op_indices?: [int]`, `force?=false`, `executor_kind?`, `executor_id?` | `{applied:[{op_index, op, result}], remainder_delta_id, warnings}` — `applied[].op_index` counts within the selected ops, not the delta's own numbering |
| `reject` | `delta_id`, `reason?`, `executor_kind?`, `executor_id?` | `{delta_id, status:"rejected", warnings}` |

**Allowed ops:** `upsert_item`, `link`, `unlink` and `set` only, using the same shapes as `trace_update`. `record` and `clear_suspect` are rejected before validation, because they record facts that have already happened and do not wait for approval. `set_layer_status` is rejected as well.

**Lifecycle:** `pending` becomes `applied` or `rejected`. Both outcomes are terminal.

- `create` (or `trace_update` with `propose: true`) runs the full `trace_update` dry-run validation, builds the previews, and stores the ops, the previews and each target item's current definition hash (`baseline_hashes`). If validation fails, no delta file is created.
- `create` returns the dry run's `warnings` (for example `waiver_added`, unknown `attrs` keys, `acceptance_dropped`), so a waiver in a proposal is visible before anyone applies it.
- Staleness: `stale` is `true` when any stored baseline hash no longer matches the item's current hash. It is computed fresh on every `list` and `apply`. A partial `apply` (`op_indices`) checks only the items the selected ops target.
- `apply`: only a pending delta can be applied. A stale delta is refused unless `force: true` (then it proceeds with a warning). The selected ops go through the real `trace_update` write path, with the same validation, category order and failure behaviour.
- `op_indices` (partial apply): 0-based positions in the delta's own `ops` array; omitted means all, out of range is an error. On success the delta becomes `applied` and never returns to pending; the unselected ops move to a **new** pending delta, renumbered from 0, returned as `remainder_delta_id`.
- `reject`: final. A rejected delta can never be applied.

---

## Reports

### handoff_report

Generates Markdown reports from Handlebars templates and runs them through a review and approval workflow.

**Writes:**

- `generate` writes `.handoff/reports/<report_id>.md` (the body) and `<report_id>.json` (the metadata).
- Report types that read trace data call `handoff_trace_report` internally, so they can also resync documents and refresh `_trace_report.json`. These are `verification`, `inspection`, `weekly`, `monthly`, `milestone`, `defect` and `completion`.
- `submit`, `approve`, `reject` and `publish` rewrite the metadata file.
- `list` and `get` are read-only.

```
handoff_report(action: "generate"|"templates"|"list"|"get"|"submit"|"approve"|"reject"|"publish", ...)
```

| Action | Arguments | Returns |
|---|---|---|
| `generate` | `report_type`, `scope?`, `data?`, `sections?`, `report_template?` | `{report:<meta>, custom_templates:[name], section_warnings?:[...]}` |
| `templates` | none | `{builtin:[name], partials:[name], custom:[{name, path, overrides_builtin}]}` |
| `list` | `report_type?`, `status?` | `{reports:[<meta>], warnings}` (newest first) |
| `get` | `report_id` | `{report:<meta>, body}` |
| `submit` | `report_id`, `comment?` | `{report:<meta>}` |
| `approve` | `report_id`, `reviewer?`, `comment?` | `{report:<meta>}` |
| `reject` | `report_id`, `reviewer?`, `comment` (required reason) | `{report:<meta>}` |
| `publish` | `report_id`, `comment?` | `{report:<meta>}` |

**Metadata (`<meta>`):** `{report_id, report_type, scope, version, status, generated_at, reviewer, approved_at, output_path, template?, revision_history:[{ts, from?, to, actor?, comment?}]}`.

- `report_id` has the form `<type>-<YYYYMMDD>-<HHMMSS>`, with a `-N` suffix on collision.
- `version` is 1 plus the number of existing reports with the same type **and identical scope**.
- `output_path` is relative to `.handoff/`.

**Custom templates (`report_template`).** `generate` renders with `.handoff/templates/<name>.md.hbs` when that file exists, else with the built-in template of that name; anything else is an error listing both (nothing is written). `report_type` still decides which data is collected, and the template sees `{report, data}`. The name is stored on the metadata as `template`. It is not the `template` argument, which is the Typst template of `format: "pdf"`. `action: "templates"` lists the built-in templates, the built-in partials and the project templates.

**Composing a report (`sections`).** An ordered array of entries that rebuilds the rendered report. The title block and a `Warnings` section are always kept.

| Entry | Shows |
|---|---|
| `{type: "metrics"}` | the headline numbers (`Summary`, else `Overview` / `Schedule`) |
| `{type: "charts"}`, `{type: "trace_diagram"}` | the Mermaid charts / the traceability diagram; they are only generated when an entry shows them |
| `{type: "ai_summary"}`, `ai_analysis`, `ai_risks`, `ai_recommendations`, `ai_sections` | the `data.ai_*` sections (`ai_sections` = all four) |
| `{type: "task_list"}` | `Completed Tasks`, `In Progress`, `Blockers`, `Tasks`, `Defects` (whichever the report has) |
| `{type: "section", heading: "Next Week"}` | any section of the report by heading (case-insensitive) |
| `{type: "custom", title?, body?, template?}` | literal Markdown and/or a registered template rendered with `{report, data}` |

`sections` supersedes `include_charts` / `include_trace_diagram`: setting one of them to `false` together with the entry that shows it is an error. An entry that matches nothing is listed in `section_warnings` with the headings the report does have.

**Scope fields:** `label`, `period`, `from`, `to`, `layers`, `items`, `campaign`, `statuses`, `assignee`, `milestone`. An unknown key is an error.

- `period` is either an ISO week (`2026-W41`, Monday to Sunday), an inclusive date range (`2026-10-05..2026-10-11`), or, for `monthly` only, a calendar month (`2026-10`).
- `period` is mutually exclusive with `from` + `to`, and `from` and `to` must be given together.
- The resolved `from`/`to` are stored back on the report's scope.

| `report_type` | Accepted scope | Preconditions | Contents (collected automatically) |
|---|---|---|---|
| `verification` | `layers`, `items`, `campaign`, `statuses` (`pass|fail|blocked|waived|pending`) | `campaign`, when given, must exist | Summary counts, per-layer coverage, results with evidence links, failed/blocked items with follow-up tasks, waived items with reasons, and an approval block. Rows come from the campaign checklist, or from every item's latest run, `waive-verify` waiver or graph state. |
| `weekly` | `period`, or `from`+`to` (default: the current ISO week) | none | Tasks completed in the period, tasks in progress (with remaining hours), blocked tasks, hours logged against estimates, per-layer verification progress, milestones, next week's schedule. A trace failure is rendered rather than failing the call. |
| `inspection` | `campaign` (**required**), `label` | The campaign must be **`approved`**. Any other scope field is an error. | Inspection certificate: document number `INSP-<campaign id>`, version, issue date, inspectors (the checklist verifiers), approver, overall judgement (`pass|fail|incomplete`), revision history, the verdict per item with evidence, non-conforming items with follow-up tasks and a blank disposition column, waived items, and a signature block. |
| `effort` | `period`, `from`+`to` (default: the whole log), `assignee` | none | Hours by week and day, by task and by assignee (with share), plus estimate-vs-actual variance, largest first. |
| `monthly` | `period` (month, week or range), or `from`+`to` (default: the current month) | none | The weekly roll-up for the month, a per-ISO-week breakdown, and the trend of daily metrics snapshots (first to last). |
| `milestone` | `milestone` (**required**), `label`. Any other field is an error. | The milestone must exist in `config.toml` or in some task's `schedule.milestone` | Planned vs actual date with a status (`achieved|achieved_late|overdue|on_track|no_date|no_tasks`), tasks with delays and hours, bug counts and open bugs, and project verification state. |
| `defect` | `period`/`from`/`to` (by bug `created_at`), `assignee`, `layers`, `items` | `layers`/`items` need a buildable trace graph | The defect register (described below). |
| `completion` | `label` only | The trace report's `project_status` must be **`complete`** (every layer `approved`). Otherwise the call is refused and nothing is written. | Project dates and task totals, per-layer verification, milestones, the defect register, effort vs estimate by assignee, and the approved reports. |

- `effort` and `monthly`, like `inspection`, `milestone`, `defect` and `completion`, reject scope fields they do not use.
- `verification` and `weekly` reject them too: `verification` takes `layers`, `items`, `campaign`, `statuses`; `weekly` takes `period` or `from`+`to`.
- **`milestone` report data:** the milestone's tasks are those whose `schedule.milestone` equals `scope.milestone`. Sections of the rendered report:
  - Milestone header: planned vs actual date, variance in days and `status`.
  - Task stats and per-task rows (status, assignee, due date, estimate/actual hours, delay days).
  - Requirement coverage (`requirements`): requirements whose effective milestone (item `- milestone:`, else its document's) is this one, with `dev_stage` counts (`by_stage`) and whether each has code (`impl`) and test (`test`) references. `available: false` plus a warning when the requirement documents could not be read.
  - GUI section (`gui`): `{has_vrt, total, done, tasks:[{id, title, requirement, status, golden, vrt, ai_visual}]}`, built from the milestone's `vrt`-labelled tasks (see `trace_tasks` VRT tasks). `has_vrt` is `false` when there are none, and the section is omitted from the rendered report. `golden` is `true` once the task's first done criterion is checked; `vrt` and `ai_visual` are `PASS` or `PENDING` from the second and third criterion. `requirement` is the stable_id parsed from the `[VRT] <stable_id> <title>` title (`null` when the title does not follow it).
  - Quality (`quality`): bug counts `{total, open, closed}` and the open bugs with priority and assignee.
  - Schedule risks (`schedule_risks`): open tasks whose due date falls after the milestone's planned date, worst first, with `overdue_days`. Empty when the milestone has no valid planned date.
  - Project verification state from the trace graph (per-layer status and counts). It is project-wide, not narrowed to the milestone.
- The verification report's coverage rate is `pass / (total - waived)`, the same `covered / (total - na - waived)` formula the trace report and the baseline diff's `regression` use. `pass_pct` and the other `*_pct` fields stay plain shares of the total.
- An `inspection` certificate titles every item with the wording frozen in the campaign checklist (what was inspected). An item whose current definition differs is marked `definition changed after inspection` and listed, with both wordings, under "Definition Changes After Inspection".

**Defect register and bug labels:**

- A bug is a task labelled `bug`.
- Its disposition comes from a `bug:<fix|defer|waive|not_a_bug>` label. A bug without one is `undecided`.
- Severity is the task's priority.
- Linked items come from the task's requirement links.
- The register counts bugs by status, severity and disposition. It also lists the currently failing items with their bug tasks, and counts failing items that have no open bug.

**`data` overlay:** keys in `data` are merged over the collected data at the top level. Use this to override or add values.

**Templates:**

- Built-in templates (`<report_type>.md.hbs`) are embedded in the binary.
- A file `.handoff/templates/<report_type>.md.hbs` overrides the built-in of the same name. Any other `*.md.hbs` file there is also registered, so it can be used as a partial.
- A custom template that fails to compile is an **error**. The server never falls back to the built-in silently.
- Templates receive `{report: <meta>, data: <collected data>}`.
- Output is not HTML-escaped.
- Helpers:
  - `{{cell x}}`: escapes `|` and turns newlines into `<br>`, for table cells.
  - `{{percent x}}`: prints `50%` or `66.7%`, or `-` when the value is missing.
  - `{{signed x}}`: prints `+2.0` or `-1.5`, or `-` when the value is missing.

**Approval flow:**

| From | Action | To |
|---|---|---|
| `draft` | `submit` | `submitted` |
| `revision_requested` | `submit` | `submitted` |
| `submitted` | `approve` | `approved` (sets `reviewer` and `approved_at`) |
| `submitted` | `reject` | `revision_requested` |
| `approved` | `publish` | `published` (terminal; `reviewer` and `approved_at` are kept) |

- Any other transition is an error and changes nothing.
- `reviewer` defaults to the calling agent's id. If neither is known, it is required.
- `published` is reached only through `publish` from `approved`; nothing leaves it.

```json
{"action": "generate", "report_type": "verification", "scope": {"layers": ["acceptance"], "statuses": ["fail", "blocked"]}}
```

---

## CLI equivalents

The CLI form is `handoff-mcp <group> <action> --flag value`.

- Flags are the tool's argument names in kebab-case (`--task-id`, `--gap-kinds a,b`).
- Arrays are comma-separated, and objects and arrays of objects are passed as JSON strings (`--ops '[...]'`, `--scope '{...}'`).
- Multi-action trace tools take `--action`.

| Tool | CLI |
|---|---|
| `handoff_trace_report` | `handoff-mcp trace report` |
| `handoff_trace_slice` | `handoff-mcp trace slice` |
| `handoff_trace_tree` | `handoff-mcp trace tree` (filters: `--filter-state`, `--filter-priority`, `--filter-suspect`, `--gaps-only`, `--filter-kind`; extra `--format json|text`) |
| `handoff_trace_matrix` | `handoff-mcp trace matrix` (`--output FILE` maps to `output_file`) |
| `handoff_trace_next` | `handoff-mcp trace next` |
| `handoff_trace_impact` | `handoff-mcp trace impact` |
| `handoff_trace_lint` | `handoff-mcp trace lint` |
| `handoff_trace_suspect` | `handoff-mcp trace suspect --action list|clear|baseline` |
| `handoff_trace_propose` | `handoff-mcp trace propose` |
| `handoff_trace_scaffold` | `handoff-mcp trace scaffold` |
| `handoff_trace_tasks` | `handoff-mcp trace tasks` (`--layers`, `--gap-kinds` and `--dev-stage` nest into `select`) |
| `handoff_trace_update` | `handoff-mcp trace update --ops '[...]'` |
| `handoff_trace_record` | `handoff-mcp trace record --results '[...]'` |
| `handoff_trace_ingest` | `handoff-mcp trace ingest` |
| `handoff_trace_history` | `handoff-mcp trace history` |
| `handoff_trace_test_run` | `handoff-mcp trace test-run --action ...` |
| `handoff_trace_baseline` | no CLI (MCP only) |
| `handoff_trace_delta` | no CLI (MCP only) |
| `handoff_report` | `handoff-mcp report generate|list|get|submit|approve|reject` (the action word becomes `action`) |

`handoff-mcp trace lint` exit codes:

| Code | Meaning |
|---|---|
| 0 | No finding at or above `--fail-on` (default `error`) |
| 1 | At least one finding at or above `--fail-on` |
| 2 | Usage or configuration error: unknown rule id, invalid `--fail-on`, or any other handler error |

`handoff-mcp trace tree` also exits with 2 on invalid parameters. With `--format text`, lint prints the rendered text instead of JSON.
