# `trace_lint` rule reference

`handoff_trace_lint` (CLI: `handoff-mcp trace lint`) checks the whole trace
graph against a fixed set of built-in rules plus your project's
`[[trace.lint.require]]` policy rules. It is **read-only**: a layer document
that was edited directly is re-parsed in memory for this call only, and
nothing is written. The full signature is in `tools.md`. Severity overrides
and policy rules are configured as described in `config.md`. The coverage
and state terms used below are defined in `verification.md`, and the suspect
terms in `change-management.md`. How to author items so these rules stay
quiet is covered by the `handoff-requirements` skill.

```
handoff_trace_lint(action?: "lint", rules?: [id], fail_on?: "error"|"warning", format?: "json"|"text", limit?)
-> {findings: [{rule, severity, item, task, doc, message}], counts: {error, warning, info}, exit_code, warnings, text?}
```

## 1. Built-in rules

Every rule can be re-leveled or switched off in `[trace.lint.rules]`.

### Structure (coverage gaps)

| rule | default | fires when | how to fix |
|---|---|---|---|
| `unverified` | warning | a left-side item's pair layer is in use, but the item has no valid verifier and is not inline-verified (`test:`/`method:` on itself). A composite tree node without its own acceptance criterion or verifier is judged by its children and fires only when none of them is covered | write a verifying item (`verifies:`), add an acceptance block under a profile with implicit acceptance, or `- waive-verify: <reason>` |
| `unrefined` | warning | a left-side, non-composite item has no refining child and no implementing task | add an item on the next layer with `refines:`, link a task (`handoff_update_task(requirement_ids=[...])`), or `- waive-refine: <reason>` |
| `orphan` | warning | a left-side item has no `refines:` even though an upper left-side layer is in use, or a right-side item has no `verifies:` | add the missing upstream link, or mark it intentional with `- derived: <reason>` |
| `task_unlinked` | warning | a task links a layer document but none of its items (`task` = the task id) | link the specific items with `requirement_ids`, or drop the document link |
| `dangling` | error | a `refines:`/`verifies:` target does not exist | fix the ID or create the target item |
| `invalid_link` | error | `refines:` does not point at a strictly higher left-side item, or `verifies:` does not point at a left-side item at the same level as the verifier or an upper one | point the link at the correct layer, or move the item |
| `cycle` | error | `refines:` links loop back on themselves | break the loop |
| `duplicate_id` | error | the same item ID is defined in more than one document | rename one of them. IDs are project-unique |

### Change (suspects)

| rule | default | fires when | how to fix |
|---|---|---|---|
| `suspect_link` | warning | an upstream item changed after a `refines:`/`verifies:` link to it was baselined | review the downstream item, update it if needed, then `trace_suspect(action="clear")` |
| `suspect_task` | warning | an item changed after a task's link to it was baselined | review the task, then clear the suspect |
| `stale_result` | warning | the latest `pass` was recorded against an earlier definition of the item | re-run the check and record a new result |
| `unbaselined` | info | a link or task link has no baseline yet, so it can never become suspect | `trace_suspect(action="baseline")` |
| `unwanted_coverage` | info | a verifier comes from a layer outside the item's `needs`/`default_needs`. It is excluded from coverage | move the verifier to a needed layer, or widen `needs` |

### Tailoring (waivers, acceptance criteria, profiles)

| rule | default | fires when | how to fix |
|---|---|---|---|
| `waiver_on_na` | warning | a `waive-verify`/`waive-refine` sits on an axis whose layer is not in use, so it has no effect | remove the waiver |
| `unlabeled_acceptance` | warning | an acceptance bullet has no `ACn:` label and was given a positional one | write explicit `AC1:`, `AC2:` … labels |
| `invalid_waiver` | warning | a `waive-verify`/`waive-refine` has an empty reason and was ignored | give a reason or delete the line |
| `unknown_acceptance_ref` | warning | `verifies: X#ACn` names an acceptance label that `X` does not declare (it falls back to verifying the whole item) | fix the label |
| `redundant_waiver` | info | the item is already covered or partial on a waived axis, so the waiver is unused. For a composite, "covered" means through its children: the waiver stands while no child is covered | remove the waiver |
| `layer_outside_profile` | info | the item's layer is not in its effective used-layer set, so it is left out of coverage and state | change the profile or `layers`, or move the item |

### Drift (stored data versus documents and tasks)

| rule | default | fires when | how to fix |
|---|---|---|---|
| `unsynced_body` | warning | a layer document was edited directly, or a parse-affecting config key changed, since its last sync (`doc` = slug) | run `trace_report` (or re-save the document) to persist the resync |
| `task_link_dangling` | warning | a task links an item ID that does not exist | fix or remove the task's `requirement_ids` entry |
| `attribute_after_body` | warning | an attribute line (`- priority:` etc.) comes after body text and was ignored | move it into the first bullet block right after the heading |
| `task_ids_drift` | info | an item's or document's stored task ids disagree with the task side | item-level: run `trace_report`/`trace_slice`, which repair it from the task side. Document-level: set the list explicitly with `doc_save(task_ids=...)` |
| `orphaned_legacy` | info | an item ended up under "(orphaned legacy items)" because its section heading no longer exists | restore the heading or remove the item |
| `orphan_run` | info | a recorded test run refers to an item ID that no longer exists | ignore it, or re-record under the new ID |
| `id_like_heading` | info | a heading looks like an ID but uses a prefix the layer does not allow (for example `HTTP-2`), so it was left as a plain heading | add the prefix in `[trace.id_prefixes]` or reword the heading |

### Format

| rule | default | fires when | how to fix |
|---|---|---|---|
| `frontmatter_invalid` | error | a document's frontmatter cannot be parsed, so its items are missing from the graph | `handoff_doc_repair_frontmatter(dry_run=true)`, then `dry_run=false` |

### Quality (string matching on the item **title** only)

| rule | default | fires when | how to fix |
|---|---|---|---|
| `ambiguous_word` | info | the title contains a vague term: 適切に, など, 必要に応じて, ユーザーフレンドリー, できるだけ, 通常, or appropriate, etc., as needed, user-friendly, as much as possible, usually, normally (English matched case-insensitively) | state a measurable criterion instead |
| `missing_acceptance` | info | a `requirement`-layer item has no acceptance-criteria block | add `受入基準:` / `Acceptance criteria:` with bullets |
| `passive_voice_hint` | info | the title contains される/られる, or English is/are/was/were followed by a past participle | name the actor ("The system shall …") |

### Requirement tree (`parent` decomposition)

Having several parents is allowed (the tree is a DAG), so there is no
"multiple parent" rule.

| rule | default | fires when | how to fix |
|---|---|---|---|
| `parent_cross_layer` | error | `parent:` points at an item on another layer | use `refines:` across layers |
| `parent_dangling` | error | `parent:` names an ID that does not exist | fix the ID |
| `tree_cycle` | error | `parent:` edges loop | break the loop |
| `self_parent` | error | an item lists itself as its parent | remove it |
| `empty_group` | warning | `kind: group` with no children | add children or drop the `kind` |
| `kind_conflict` | warning | `kind: leaf` on an item that has children (it is treated as a composite) | fix `kind` or move the children |
| `condition_missing` | warning | a child of a `decision`/`select` has no condition label | add `- condition: <label>` (or `condition@<parent>:`) |
| `condition_duplicate` | warning | two children of one branch node share a label (trimmed, case-insensitive) | make the labels unique |
| `condition_on_non_branch` | warning | a condition label sits on an edge to a parent that is not a branch | remove the label, or make the parent `decision`/`select` |
| `attr_unknown_parent` | warning | `order@X`/`condition@X` names an X that is not in the item's `parent:` list | fix X or add it to `parent:` |
| `decision_arity` | warning | a `decision` has more than two children | change it to `kind: select` |
| `empty_branch` | warning | a `decision`/`select` has no children | add the branches |
| `invalid_order` | warning | `order`/`order@<parent>` is not a non-negative integer | fix the value |
| `dotted_id` | warning | the heading writes the ID with dots (`DSP-1.2.3`), which parses as a shorter ID | use dashes: `DSP-001-002` |
| `leaf_without_acceptance` | warning | a left-side leaf has no acceptance criteria, no verifier, no inline `method`/`test`, and no `waive-verify` | add one of them |
| `shallow_leaf` | info | a leaf's body joins behaviors with かつ, および, また、, 並びに, or English and, also, as well as (whole words) | split it into child items |
| `id_parent_mismatch` | info | an ID is named like a child of a same-layer item (`REQ-001-002` under `REQ-001`) but does not list it in `parent:` | add `- parent: REQ-001` or rename |

### Parse-time rules only fire for out-of-sync documents

Four rules come from parser warnings: `unlabeled_acceptance`,
`invalid_waiver`, `id_like_heading`, and `attribute_after_body`.
`trace_lint` only sees those warnings for documents it re-parses during the
call, which means documents that were edited directly or are out of sync.
A document saved through `doc_save`/`doc_update_section` is already synced,
so its parser warnings appear **in that call's `warnings`** and not in
later lint runs. Read the `doc_save` response. Do not count on lint to catch
these afterwards.

## 2. Policy rules (`[[trace.lint.require]]`)

Each policy rule from `config.md` §7 runs alongside the built-ins under its
own `id`. Every item that matches the `when` filter and fails the `need`
produces a finding with the message `<item> does not satisfy <id> (<need>)`.
The default severity is `error` unless the entry sets `severity`. A
`[trace.lint.rules]` entry for the same id overrides both. The `rules` filter
accepts these ids too.

## 3. Severity, `fail_on`, `rules`, `limit`

- **Severity**: built-in default, then the `[trace.lint.rules]` override.
  `"off"` removes the rule's findings entirely.
- **`fail_on`** (`"error"` by default, or `"warning"`): `exit_code` is `1`
  when any finding is at or above this severity, otherwise `0`. `info`
  findings never fail a run.
- **`rules`**: run only these rule or policy ids. An unknown id, an empty
  array, or a value that is not an array is a usage error.
- **`limit`**: truncates `findings`. `counts` and `exit_code` still reflect
  every match, and a `findings truncated to limit=N of M matching entries`
  warning is added.

## 4. Output

```json
{
  "findings": [
    {"rule": "unverified", "severity": "warning", "item": "REQ-001", "task": null, "doc": "auth-req", "message": "no valid verifier and not inline-verified"}
  ],
  "counts": {"error": 0, "warning": 1, "info": 0},
  "exit_code": 0,
  "warnings": []
}
```

- Findings are sorted deterministically: severity descending, then rule id,
  then item in natural order (`FR-2` before `FR-10`).
- `item`, `task`, and `doc` are `null` when they do not apply. Parse-time
  and `unsynced_body` findings carry only `doc`. `task_unlinked` carries
  only `task`.
- `format: "text"` adds a `text` field with one line per finding:
  `severity[rule] subject: message`.

### `findings` versus `warnings`

- **`findings`** are the result: content problems in your trace graph. Fix
  them or tune their severity.
- **`warnings`** are about the call and the config. They include:
  - disabled `[[trace.layer]]` declarations and profile-resolution problems
  - the "`layers` and `profile` both set" notice
  - unreadable documents
  - parser messages from the in-memory resync
  - the truncation notice

  They never affect `exit_code`, but they often explain an empty or
  surprising result. Read them first.

## 5. CLI exit codes

`handoff-mcp trace lint [--rules a,b] [--fail-on warning] [--format text] [--limit N]`

| code | meaning |
|---|---|
| `0` | no finding at or above `fail_on` |
| `1` | at least one finding at or above `fail_on` |
| `2` | usage or config error: an unknown rule id in `--rules`, an invalid `--fail-on`/`--format`, an unparseable `config.toml`, a `[trace.lint.rules]` entry with an unknown id or value, a `[[trace.lint.require]]` entry with a missing `id` or a missing/unknown `need`/`severity`, or any other failure |

`handoff-mcp trace tree` also exits 2 on bad arguments; other subcommands use 0/1. The `2` exists so CI can tell
"policy violated" apart from "lint could not run".

## 6. `action="quality_prompt"`: LLM-judged quality

Some quality checks need judgment that string matching cannot give. This
mode returns **prompt templates** for nine ISO/IEC/IEEE 29148
characteristics, plus one set-level check (`completeness_set`). The server
never calls an LLM itself.

```
handoff_trace_lint(action: "quality_prompt", items?: [id], aspects?: [aspect])
-> {prompts: [{item_id, title, statement, acceptance, text, aspects: [{name, prompt_template, context}]}], warnings}
```

Per-item aspects (the default set of nine):

| aspect | checks that the requirement … |
|---|---|
| `singular` | states one capability, constraint, or condition (no bundling with and/or) |
| `verifiable` | can be verified by inspection, analysis, demonstration, or test |
| `unambiguous` | has exactly one interpretation, with no vague qualifiers |
| `complete` | needs no further conditions, exceptions, or responses |
| `feasible` | can be realized within technical, schedule, budget, and legal limits |
| `traceable` | traces back to its origin and forward to design and test |
| `necessary` | is essential to the system scope (removing it would reduce the system's ability to meet its goals) |
| `implementation_free` | says what the system shall do, not how (no technology, algorithm, or internal-structure choices) |
| `consistent` | does not contradict another requirement in the same set |

Set-level aspect (opt-in only):

| aspect | checks that the requirement set … |
|---|---|
| `completeness_set` | as a whole covers the stakeholder goals, boundary conditions, and error scenarios; missing requirements are as dangerous as incorrect ones |

- `items` left out means every `requirement`-layer item. Explicit `items`
  may be on any layer.
- `aspects` left out means the nine per-item aspects (never
  `completeness_set`). An unknown aspect, or an empty `items`/`aspects`
  array, is a usage error.
- `completeness_set` is **exclusive**: pass it alone. Mixing it with a
  per-item aspect is a usage error. It returns one prompt for the whole
  set instead of one per item:

  ```
  handoff_trace_lint(action: "quality_prompt", aspects: ["completeness_set"])
  -> {prompts: [{scope: "set", items_included: N, item_ids: [...], text,
                 aspects: [{name: "completeness_set", prompt_template, context}]}], warnings}
  ```

  `text` lists every selected item (`[id]`, title, statement, acceptance
  criteria). Use `items` to judge a subset, such as one feature area. Fill
  `{text}` in the template, send it to an LLM, and turn each gap it names
  into a new requirement candidate.
- `prompt_template` contains a literal `{text}` placeholder. Per-item
  `text` is the whole definition as it is on disk: the title, the statement,
  and an `Acceptance criteria:` list. Substitute it into the template.
- Each template asks for `pass`/`fail`, a one-sentence reason, and a
  suggested rewrite on fail.

Intended flow:

1. Call `quality_prompt` for the items you are reviewing.
2. Fill each template and evaluate it yourself, or hand it to a sub-agent.
3. For every `fail`, fix the item text: edit the body and `doc_save`, or use
   `trace_update`'s `upsert_item`. Run `trace_lint` again for the
   string-matching rules.

The server has **no store for quality verdicts**. Report them in your answer
or task notes, or record a recurring pattern as project memory. Editing an
item changes its definition hash, so downstream links may become suspect
(see `change-management.md`).
