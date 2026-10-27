---
name: handoff-requirements
description: "Authoring V-model items — how to write requirements, specs, acceptance criteria and test items in layer documents: IDs, attribute blocks, refines/verifies links, waivers, and requirement-tree decomposition (parent/kind/condition/order). Triggers on '要件を起票', '要件を書く', '仕様を書く', 'テスト項目を追加', '受入基準', '要件ツリー', '要件分解', 'decision ノード', 'select ノード', 'write a requirement', 'add a spec item', 'acceptance criteria', 'decompose requirements', 'requirement tree', 'GUI要件', 'GUI requirement', 'mock属性', 'ゴールデン', 'viewport', 'capture_method', or whenever you are about to create or edit an item in a document that has a `layer`."
---

# Handoff Requirements Skill (authoring V-model items)

How to **write** items in V-model layer documents so the traceability engine
reads them the way you intend. For the system overview, setup, verification,
change tracking and the full tool reference, see the `handoff-trace` skill.

- Per-layer copy-paste templates: [references/templates.md](references/templates.md)
- Decomposing requirements into a tree (`parent` / `kind` / `condition` /
  `order`, decision and select branches): [references/requirement-tree.md](references/requirement-tree.md)
- Finding non-functional requirements systematically (ISO/IEC 25010 checklist,
  questions per quality characteristic, how to file `NFR-nnn` items):
  [references/nfr-checklist.md](references/nfr-checklist.md)
- Gate checklists to pass before moving to the next requirements phase (six
  gates, deliverables, quality bar, tool commands):
  [references/preflight-checklists.md](references/preflight-checklists.md)

## 0. Before you write

1. **Is the V-model on?** A fresh `handoff_init` writes no `[trace]` section.
   Check `handoff_get_config` (or `.handoff/config.toml`) for `[trace] profile`
   / `layers`. If neither is set, follow the `handoff-trace` skill's
   "First-time setup" first — otherwise the item you write may be counted
   against the wrong set of layers.
2. **Which profile?** It decides which layers you must fill:

   | profile | layers you write | acceptance criteria become verification items automatically? |
   |---|---|---|
   | `minimal` | requirement (+ acceptance) | yes |
   | `standard` | requirement, basic_spec, acceptance, system_test | no — write explicit test items |
   | `full` | all six | no |
   | `bugfix` | requirement ("再現条件"), acceptance ("回帰テスト") | yes |

3. **Check for duplicates first**: `handoff_trace_propose(title: "...")`
   returns similar existing items (`candidates`) and a ready template
   (`proposal.markdown`, `proposal.next_ids`) sized to the profile. It writes
   nothing. The proposed ID uses the layer's default prefix (`REQ-001`); for
   a scope prefix you registered (`CART`, `PAY`) pass `prefix: "CART"` to get
   the next free `CART-nnn`, or use `trace_propose(parent: ...)`, which numbers
   children after their parent (`CART-001-003`; under a `decision`/`select`
   parent the template carries a `- condition: TBD` line to give a label).
   Every fill-in slot in the template is the marker `TBD` — replace them.

## 1. Pick the layer and the document

| You are writing… | layer | side | default ID prefixes |
|---|---|---|---|
| What the user/stakeholder needs | `requirement` | left (definition) | `REQ`, `FR`, `NFR` |
| How the system satisfies it (external behaviour, interfaces) | `basic_spec` | left | `SPEC`, `BS` |
| Implementation-level design (modules, data, algorithms) | `detailed_spec` | left | `DS` |
| How we accept a requirement | `acceptance` | right (verification) | `AT`, `AC` |
| A system/integration test for a basic spec | `system_test` | right | `ST` |
| A unit test for a detailed spec | `unit_test` | right | `UT` |

- For non-functional requirements (`NFR-nnn`), walk
  [references/nfr-checklist.md](references/nfr-checklist.md) instead of
  inventing them one at a time; before starting the next requirements phase,
  run the matching gate in [references/preflight-checklists.md](references/preflight-checklists.md).
- A document becomes a **layer document** when saved with
  `handoff_doc_save(slug=..., title=..., layer="requirement", body=...)`.
  From then on its Markdown body is the source of truth: every save re-parses
  the body into items. Edit the body; don't try to add items with
  `doc_verify(add_item)` (refused on layer documents).
- One document per layer per feature (or per tree branch) is the normal
  shape. A single item may override its document's layer with `- layer: <id>`
  — useful to keep a requirement and its paired acceptance item side by side.
- Project-specific prefixes (scopes like `SYS`, `HW`, `UI`) must be registered
  or the heading is not recognized as an item:
  ```toml
  [trace.id_prefixes]
  requirement = ["SYS", "HW", "UI"]
  ```
  These are added to the defaults, never replacing them.

## 2. IDs

A heading is an item when its text **starts with an ID**:

```
<PREFIX>-<digits>[a-z]          REQ-003, AT-001a
<PREFIX>-<seg>-...-<digits>[a-z] ST-LOGIN-01, DSP-001-002
```

- `PREFIX` = any registered prefix (built-in or `[trace.id_prefixes]`, of
  **any** layer); segments are `-`-separated alphanumerics; the **last**
  segment is digits with at most one trailing lowercase letter.
- The item takes the **document's** layer regardless of its prefix: an
  `### AT-001` heading inside a requirement document silently becomes a
  requirement item. Give such an item `- layer: acceptance`, or put it in an
  acceptance document.
- A `:` or `.` right after the ID is a separator: `### REQ-003: Account lock`.
- Heading level does not matter (`##`, `###`, `####` all work). Headings inside
  fenced code blocks are ignored.
- IDs are **project-unique** and are the stable identity of the item — links,
  task links, results and history all key on them. **Never renumber or reuse
  an ID**; retire an item by deleting it, and create a new ID for new meaning.
- **No dots**: `DSP-1.2.3` is parsed as ID `DSP-1` with title "2.3 …"
  (`dotted_id` lint). Use `DSP-001-002-003`.
- Child IDs follow the parent: children of `DSP-001` are `DSP-001-001`,
  `DSP-001-002`, … (`trace_propose(parent: "DSP-001")` numbers them for you).
- An ID-looking heading with an unregistered prefix (`HTTP-2`, `RQ-VGAP-001`)
  is *not* an item — the save response warns (`id_like_heading`). Register the
  prefix if you meant it.

## 3. Item anatomy — heading → attributes → body (strict order)

```markdown
### REQ-003 Lock the account after repeated login failures

- priority: P1
- rationale: deter brute-force attacks (security audit finding)

Lock an account for 15 minutes after 5 consecutive failed logins.

受入基準:
- AC1: Given 4 failed attempts When the 5th attempt fails Then the account is locked
- AC2: WHEN the account is locked THE SYSTEM SHALL reject even a correct password
```

1. **Heading** — ID + short title.
2. **Attribute block** — the *first contiguous bullet list right after the
   heading*. Blank lines before it are fine; a blank line inside it ends it.
   Only `- key: value` lines with a known key are attributes; unknown keys stay
   in the body.
3. **Body** — the statement (prose, lists, code — anything).
4. **Acceptance criteria** (optional) — see §5.

**The attribute block is only recognized immediately after the heading.** If
the first non-blank thing after the heading is prose, *no* attribute of that
item is read — `- refines: REQ-001` written after the body is silently plain
text and the link never exists. The save response warns
(`attribute_after_body`) **only in that response** — a later `trace_lint`
does not repeat it for a document saved through MCP — so read the warnings
when you save, and write in the right order from the start:

```markdown
### SPEC-001 Session token format       ← WRONG
Tokens are JWTs signed with RS256.

- refines: REQ-001                       ← ignored: body already started
```

## 4. Attribute reference

| key | value | where | meaning |
|---|---|---|---|
| `refines` | `REQ-001, REQ-002` | left side | this item details a **higher-level** left item (e.g. a spec refines a requirement) |
| `verifies` | `SPEC-001` or `REQ-003#AC2` | right side | this item verifies a left item at the **same or higher** level; `#ACn` targets one acceptance criterion |
| `parent` | `DSP-001, COM-003` | any | same-layer decomposition (requirement tree; several parents allowed) — see [references/requirement-tree.md](references/requirement-tree.md) |
| `order` / `order@<parent>` | integer ≥ 0 | tree children | sibling order (per parent when several) |
| `kind` | `leaf` \| `group` \| `decision` \| `select` | tree nodes | node type |
| `condition` / `condition@<parent>` | free text | children of decision/select | branch label (`yes`/`no`, case names, `default`) |
| `layer` | layer id | any | per-item layer override |
| `priority` | `P0`–`P3` | any | priority (drives ordering in `trace_next`, lint policies) |
| `method` | `manual` \| `auto` \| `visual` \| `review` | verification items, or a left item verifying itself | how it is verified |
| `test` | test name as the runner reports it, e.g. `auth::locks_after_five_failures` (repeatable, comma-separated) — **not** a file path | verification items | the automated test(s); `trace_ingest` matches it exactly or as a `::`-suffix of the reported name |
| `needs` | layer ids, e.g. `system_test, unit_test` | left side | which verification/refinement layers this item requires — overrides the profile's default; an **empty** value means "requires nothing" |
| `assignee` | assignee key from `config.toml [assignees]` | any | who verifies/owns it; unknown keys warn; filter with `trace_next(assignee=...)` |
| `rationale` | text | any | why the item exists |
| `derived` | reason (required) | left side | intentionally has no upstream (suppresses the `orphan` gap) |
| `waive-verify` / `waive-refine` | reason (required) | left side | exempts the item from needing a verifier / a refining child |
| `from` | source id | generated items | provenance written by `trace_scaffold`; makes scaffolding idempotent — keep it |

### GUI requirement attributes (VRT)

A GUI requirement can declare its visual-regression inputs as ordinary
attribute lines (same strict position: directly under the heading):

```markdown
### REQ-010 Login screen

- mock: path/to/mockup.html         # mock the screen is verified against (VRT)
- golden: path/to/golden.png        # golden screenshot (VRT)
- viewport: 1280x720                # capture viewport, WxH, kept as written
- capture_method: playwright        # how to capture, e.g. playwright | manual
```

| key | value | meaning |
|---|---|---|
| `mock` | path | mock file for the screen. **A `mock` line is what makes `handoff_trace_tasks` generate an extra `[VRT] <id> <title>` verification task** next to the item's normal implementation task |
| `golden` | path | golden screenshot |
| `viewport` | `<W>x<H>` | capture viewport; the format is not validated |
| `capture_method` | free name | capture method; not an enum (`playwright`, `manual`, `code-server`, … are conventions). Falls back to `config.toml [gui] default_capture_method` when the item declares none |

Rules: values are stored verbatim (trimmed); a repeated line is
last-line-wins; an empty value means "unset". `handoff_doc_req_list` returns
them as `mock` / `golden` / `viewport` / `capture_method` (keys are omitted
when unset).

Rules:

- `derived` / `waive-*` with an empty reason are **dropped with a warning** —
  nothing silently excuses itself from coverage.
- **Do not add `derived` / `waive-*` on your own as an AI.** Every new waiver
  is reported back as a `waiver_added` warning; get the user's agreement first.
- **Link validity**: `refines` must point upward to a left item with a smaller
  level; `verifies` must point from a right item to a left item at the same or
  a higher level. Anything else is an `invalid_link` gap; an unknown ID is
  `dangling`. `trace_update` rejects invalid links at write time.
- **Inline verification**: a left item with its own `method`/`test` counts as
  its own verifier — the fastest way to close the V for small items.

## 5. Acceptance criteria

```markdown
受入基準:
- AC1: Given … When … Then …
- AC2: WHEN … THE SYSTEM SHALL …
- AC3: plain sentence
```

- Trigger line: `受入基準:` / `Acceptance criteria:` / `Acceptance:`
  (case-insensitive, emphasis allowed), followed by a bullet list. The first
  such block in the item is used; it ends at the first blank line after the
  bullets. Any following non-blank, non-bullet line (indented or not)
  continues the previous criterion — leave a blank line before prose that
  follows the list.
- Label each bullet `AC<n>:`. Unlabelled bullets get a position-based label
  and a warning (`unlabeled_acceptance`) — labels are what `REQ-003#AC2`
  refers to, so make them explicit and never renumber them.
- Each criterion is classified `gwt` (Given/When/Then in order), `ears`
  (starts with WHEN/WHILE/WHERE/IF and contains SHALL) or `text`.
- Under `minimal`/`bugfix` (implicit acceptance), each criterion is
  materialized as its own verification item `REQ-003#AC1` — record results and
  link tasks against it directly; no acceptance document is needed.
- Under `standard`/`full`, write explicit verification items, or generate them
  with `handoff_trace_scaffold(doc: "<requirement doc>", target_doc: "<acceptance doc>", mode: "preview")`
  then `mode: "apply"` (one item per criterion, idempotent via `from:`). The
  target document must already exist as a layer document — create it first
  with a title-only body, e.g. `doc_save(slug: "auth-acceptance", title: ...,
  layer: "acceptance", body: "# Auth Acceptance Tests\n")`.
- A verifier with `verifies: REQ-003` covers every criterion; with
  `verifies: REQ-003#AC2` only AC2 — the others leave the requirement
  `partial`.
- Editing a criterion's text changes the item's definition hash: verifiers and
  linked tasks become **suspect** and an `approved` item drops back to `draft`
  (see `handoff-trace` → change management). Fixing typos is still a change.

## 6. Writing workflow

### Deriving requirements from a use case

Use cases are a **discovery tool** for requirements, not something the V-model
engine manages: there is no `UC-` layer, no `refines:`/`verifies:` link to a
use case, and no coverage or suspect tracking for it.

1. Describe the scenario with the Use-Case template in
   `references/templates.md` (actor, trigger, precondition, main / alternative /
   exception flows, postcondition). Keep it in a plain document (no `layer`) or
   the task notes, not in a layer document.
2. Derive requirements step by step: each main-flow step that the system must
   support becomes an `FR-`; qualities implied by the flow (latency, availability,
   security) become `NFR-`; alternative and exception flows become further
   requirements or acceptance criteria.
3. File the derived requirements as ordinary requirement-layer items (steps 1-5
   below). Do not reference the `UC-` id as a `parent`/`refines` target.

**Standard workflow**

1. **Propose** — `handoff_trace_propose(title: "...", notes: "...")` (or
   `task_id: "..."`, or `parent: "<id>"` for a tree child). Review
   `candidates`; reuse an existing item instead of duplicating it.
2. **Write** — either edit the body and save the whole document:
   ```
   handoff_doc_save(doc_id: "<id>", body: "<full markdown>")
   ```
   or append one section without resending the body
   (`handoff_doc_save(doc_id, append_body: "### REQ-004 ...")`), or upsert
   items structurally (best for AI batch edits):
   ```
   handoff_trace_update(ops: [
     {op: "upsert_item", doc: "auth-requirements", id: "REQ-004",
      title: "Password reset link expiry",
      statement: "Reset links expire after 30 minutes.",
      acceptance: [{label: "AC1", text: "Given a link older than 30 min When it is opened Then it is rejected"}],
      attrs: {priority: "P1", parent: "SYS-001"}}
   ], dry_run: true)
   ```
   `dry_run: true` shows a diff per item; re-run without it to write. All ops
   are validated before anything is written. On an existing item, `attrs` is
   merged key by key (`needs` and `assignee` are kept unless you set them in
   `attrs`; an unknown `attrs` key is ignored but listed in `warnings`) — and
   **`acceptance` replaces the whole list** — pass every criterion you want to
   keep, or the omitted ones are deleted. The response warns
   (`acceptance_dropped`) and names any verifier whose recorded result is
   orphaned, but the deletion still happens, so dry-run edits of acceptance
   criteria.
3. **Read the save response** — act on, not just skim:

   | field | meaning | action |
   |---|---|---|
   | `warnings[]` | parse problems as plain text (empty waiver reason, "ID-like heading ignored", attribute after body, unlabeled criterion, invalid `order`/`kind`, duplicate ID, an id prefix of another layer without `- layer:`, an `order@`/`condition@` parent that is not in `parent:`) | fix the body — the latter is also reported by `trace_lint` (`attr_unknown_parent`) |
   | an error naming `self_parent` / `parent_dangling` / `parent_cross_layer` / `tree_cycle` | the save was **rejected** (codes listed in the error text) | fix the `parent` attribute; write parents before children |
   | `suspect_introduced` `{changed, created, approval_reset, links, tasks, reverify}` | your edit changed definitions others depend on | review with `handoff_trace_suspect(action: "list")` |
   | `orphaned_children` | you deleted an item other items used as `parent` | re-parent or delete them |
   | `scaffold_suggestions` | items that could get generated verification items | consider `trace_scaffold` |
   | `waiver_added` (trace_update) | a new `derived`/`waive-*` | confirm with the user |
   | `NEW_REQUIREMENT_DETECTED` (info) | a new leaf item with no task yet (never raised for a composite or an implicit `X#ACn` verification item) | create a task for it |
   | `suspect_introduced.created` | items this save created (a first save puts every item here, none under `changed`) | nothing to review yet — only `links`/`tasks`/`reverify` need action |
   | `suspect_introduced.approval_reset` | items whose `review`/`approved` approval went back to `draft` because their definition changed | ask for approval again |

4. **Check** — `handoff_trace_lint()` is the authoritative list (structure,
   coverage gaps such as `unverified` / `unrefined`, tree and quality rules).
   For trees, `handoff_trace_tree(layer: "requirement", filter: {gaps_only: true})`
   additionally shows defective nodes in place (uncovered/failing/suspect, tree
   findings, orphan/dangling/invalid-link/cycle/duplicate-id wiring) — it does
   not list `unrefined` (vertical) gaps or `not_run` leaves, so an empty result
   there does not mean lint is clean. For a wording review of
   the items you wrote, `handoff_trace_lint(action: "quality_prompt", items: [...])`.
5. **Connect downstream** — refine into the next layer (`refines:`), create
   verification items (`verifies:` / `trace_scaffold`), and link the
   implementing task:
   `handoff_update_task(task: {id: "t12", requirement_ids: ["REQ-004"]})`
   (or bulk: `handoff_trace_tasks(mode: "preview")` → `"apply"`).

## 7. Editing existing items

- **Before** changing a definition others depend on, run
  `handoff_trace_impact(item: "REQ-003", proposed: "### REQ-003 <title>\n\n- priority: P1\n\n<new statement>")`
  to see which links, tasks and results will turn suspect (`would_suspect`,
  `rerun_candidates`). Pass the **whole item block including its heading** —
  with only a statement the change cannot be hashed and is merely assumed.
- Changing **attributes** (priority, method, assignee, …) does not change the
  definition hash; changing title, body, acceptance criteria or a branch
  `condition` label does.
- Deleting an item: remove its heading block and save. Items that `refines`/
  `verifies` it become `dangling`; tree children are listed in
  `orphaned_children`; task links to it are reported by the next
  `trace_report`/`trace_lint`.
- Edited the `.md` file directly (outside MCP)? Run `handoff_trace_report()`
  once so the items are re-synced before using `trace_propose`/`trace_update`.

## 8. Common mistakes

| Symptom | Cause | Fix |
|---|---|---|
| Item missing from `trace_report` | prefix not registered for any layer, or document has no `layer` | register the prefix in `[trace.id_prefixes]`; `doc_save(doc_id, layer=...)` |
| Test item counted as a requirement | an `AT-`/`ST-` heading inside a requirement document without `- layer:` | add `- layer: acceptance` (or move it) |
| Link not counted (save warned `attribute_after_body`) | attributes written after body text | move the bullets directly under the heading |
| `invalid_link` | `refines` pointing sideways or down, or `verifies` targeting a right-side item or a left item deeper than the verifier (e.g. an `acceptance` item verifying a `detailed_spec` item) | refine upward; verify a left item at the same or a higher level |
| `dangling` | typo in the target ID, or target deleted | fix the ID / re-target |
| Requirement shows `partial` | some `AC<n>` has no verifier | add a verifier for that criterion or verify the whole item |
| `dotted_id` / unexpected short ID | `DSP-1.2` style IDs | dash-separated IDs |
| Everything "covered" but nothing passing (minimal) | implicit verification items exist but no results recorded | record results — check `state`, not coverage |
| Waiver silently missing | empty reason | write `- waive-verify: <reason>` |
| Tests never matched by `trace_ingest` (`missing_refs`) | `test:` written as a file path (`tests/auth.rs::fn`) or with a different module path | write the name as the runner prints it, or a `::`-suffix of it (`auth::fn`, `fn`) |

## See also

- `handoff-trace` — overview, first-time setup, verification and approval,
  change tracking, reports, full tool/config/lint references.
- `handoff-docs` — the generic document tools layer documents are built on.
