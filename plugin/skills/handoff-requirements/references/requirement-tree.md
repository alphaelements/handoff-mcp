# Requirement tree — decomposing requirements to testable leaves

`refines` links items **across** layers (requirement → basic spec → detailed
spec). The requirement tree adds `parent`, which decomposes an item into
smaller items **within the same layer**, to any depth:

```
ARCH  architecture (top)
 └─ SYS  system scope
     ├─ DSP  display feature
     │   ├─ DSP-001  key display
     │   │   ├─ DSP-001-001  redraw on state change   ← leaf (verified by its criteria)
     │   │   └─ DSP-001-002  icon rendering           ← leaf
     │   └─ DSP-002  layout
     └─ COM  communication feature
         └─ DSP-001-001      ← second parent (DAG): also required by COM
```

| relation | meaning | direction |
|---|---|---|
| `refines` | detailing across layers | different layers |
| `parent` | decomposition | same layer (several parents allowed) |

Projects that never write `parent` behave as before: every item is a root and
a leaf.

The examples below use the scope prefixes `ARCH`, `SYS`, `DSP`, `COM` and
`KEY`. Prefixes must be registered, or the headings are not items at all:

```toml
[trace.id_prefixes]
requirement = ["ARCH", "SYS", "DSP", "COM", "KEY"]
```

A `parent` must already exist when the child is saved (`parent_dangling` is
rejected), so write the tree top-down — or save parent and children in the
same document / the same `trace_update` call.

## 1. Attributes

Written in the item's attribute block (right after the heading):

```markdown
### DSP-001-001 Redraw on state change

- parent: DSP-001, COM-003
- order: 2
- order@COM-003: 1
- kind: leaf
- priority: P1

When an action's state topic changes, only the keys bound to it are redrawn.

受入基準:
- AC1: Given key 3 is bound to timer.toggle When timer.running goes false→true Then only key 3 is redrawn
```

| attribute | value | meaning |
|---|---|---|
| `parent` | same-layer IDs, comma-separated | this item is part of the decomposition of each listed parent |
| `order` | integer ≥ 0 | sibling order (smaller first) |
| `order@<parent>` | integer ≥ 0 | sibling order under one specific parent |
| `kind` | `leaf` / `group` / `decision` / `select` | explicit node type (§3) |
| `condition` | free text (trimmed) | branch label seen from a decision/select parent; default for every parent |
| `condition@<parent>` | free text | branch label for one specific parent; overrides `condition` |

- Sibling order: items with `order` first (ascending) → document path →
  position in the document. Consumers should read each parent's `children[]`
  as returned (already ordered) rather than re-sorting.
- The `<parent>` in `order@`/`condition@` must be in the item's own `parent:`
  list. `trace_update` rejects a violation; a direct body edit reports
  `attr_unknown_parent`.
- An invalid `order`/`kind` value (`-1`, `abc`, `foo`) does not block the save:
  it is treated as unset with a warning.

### Rejected at save time

`doc_save` / `doc_update_section` / `trace_update` refuse, before writing, any
change whose `parent` would be:

| code (named in the error text) | cause |
|---|---|
| `self_parent` | the item lists itself |
| `parent_dangling` | the parent ID does not exist |
| `parent_cross_layer` | the parent is on another layer — use `refines` across layers |
| `tree_cycle` | the parent edges would form a loop |

A file edited directly on disk can still contain these; `trace_lint` reports
them as errors.

## 2. Several parents (DAG)

```markdown
### DSP-001-001 Redraw on state change

- parent: DSP-001, COM-003
```

- The item appears under both parents. Each parent's subtree roll-up includes
  it (intentional double counting per parent); project totals come from
  `trace_tree`'s `totals`.
- In `trace_tree` output a shared node is expanded at its first position;
  later occurrences show `has_children: true, children: null`
  (`totals.dag_shared`, `totals.truncated_subtrees`). Text output marks shared
  nodes with `*`.
- Parents may live in other documents — IDs are project-unique.

## 3. Node kinds

| kind | when | meaning |
|---|---|---|
| leaf | no same-layer children and not `kind: group` | one verifiable behaviour; needs acceptance criteria or a verifier |
| composite | has children, or `kind: group` | state rolls up from its leaves |
| decision | `kind: decision` | 2-way conditional branch; children are the branches |
| select | `kind: select` | N-way (3+) branch; children are the cases |

- Any item with children is a composite automatically. Use `kind: group` to
  mark an intermediate node before its children exist (`empty_group` warns
  while it has none).
- `kind: leaf` on an item that has children is a contradiction
  (`kind_conflict`).
- Composites are exempt from the vertical (`refines`) requirement and from
  `task_unlinked`; their horizontal coverage rolls up from their leaves.
- A composite **without its own acceptance criteria or verifier** is verified
  through its children: its horizontal coverage is the aggregate of its
  leaves (all covered -> covered, some -> partial, none -> uncovered), and it
  gets an `unverified` gap only when **no** leaf below it is covered. Covered
  leaves therefore never leave the composite reported `unverified`.
- To verify a composite itself (the combination of its children), give it an
  **integrated acceptance criterion** and a verifier; it is then judged by
  that verification alone. `- waive-verify: <reason>` (with the user's
  agreement) silences a composite whose children are not covered.

### Decision (2-way)

```markdown
### DSP-010 Display by connection state

- kind: decision

The display switches with the connection state.

### DSP-010-001 Connected display

- parent: DSP-010
- order: 1
- condition: yes

Keys use the normal colour while connected.

受入基準:
- AC1: Given connected When the screen is drawn Then keys use the normal colour

### DSP-010-002 Disconnected display

- parent: DSP-010
- order: 2
- condition: no

Keys are greyed out while disconnected.
```

- Every branch child **must** carry a `condition` label. Convention for
  decisions: `yes` / `no` (state names are fine too).
- The label belongs to the edge from the branch parent. It shows up in
  `trace_report` as `items[].conditions` (`{parent_id: label}`) and in
  `trace_tree` as `condition` / `conditions` on the node.
- **The label is part of the child's definition hash**: swapping `yes`/`no`
  makes the child's verifiers suspect.
- A child shared by several branch parents overrides per parent:
  `- condition: yes` plus `- condition@SEL-001: case-2`.
- A decision with more than two children fires `decision_arity` — switch it to
  `select`.

### Select (N-way)

```markdown
### KEY-020 Behaviour by press type

- kind: select

### KEY-020-001 Short press

- parent: KEY-020
- order: 1
- condition: short-press

### KEY-020-002 Long press

- parent: KEY-020
- order: 2
- condition: long-press

### KEY-020-003 Anything else

- parent: KEY-020
- order: 3
- condition: default
```

- Labels must be unique among one parent's children (`condition_duplicate`,
  compared trimmed and case-insensitively).
- `default` is the reserved catch-all case; use it once.
- A branch child with no label fires `condition_missing`; a decision/select
  with no children fires `empty_branch`.

## 4. Decomposition procedure

1. **Architecture** — the topmost parent; everything hangs off it.
2. **Scope** — one item per scope prefix (`SYS`, `HW`, `DSP`, …), registered
   in `[trace.id_prefixes] requirement = [...]`.
3. **Feature** — what each scope must do.
4. **Condition branch** — where behaviour forks, a `decision` (2-way) or
   `select` (3+) node whose children are the branches, each labelled.
5. **Leaf** — one verifiable behaviour with acceptance criteria. A leaf has no
   children.

Intermediate stages may be placeholders (`kind: group`). Add children top-down.

### Leaf checklist

Before calling a leaf done, consider: normal case / boundary values / error
handling / timing / concurrency / power loss and restart / persistence /
upgrade and migration / security / localization / accessibility. State "not
applicable" explicitly rather than skipping silently. If `shallow_leaf` fires
(the body joins several behaviours with "and" / 「かつ」 / 「および」), split it.

### Getting child IDs

```
handoff_trace_propose(parent: "DSP-001", title: "Icon rendering")
```

- Proposes a same-layer child with ID `<parent>-<n>` (n = highest existing
  `<parent>-<digits>` + 1, zero-padded to 3 digits), a template that already
  carries `- parent: DSP-001`, and the parent's document as the placement.
- Fails if the parent does not exist or is not on a definition (left-side)
  layer. It also returns near-duplicate `candidates`.
- It writes nothing — fill in the template and save with `doc_save` /
  `trace_update(upsert_item)`.

## 5. One branch, one document — parallel authoring

Keep each branch (scope subtree) in its own layer document. Several authors or
agents can then decompose in parallel without editing the same file; `parent`
works across documents.

A proven agent split:

1. **Planner** — writes architecture, scope and feature nodes; assigns one
   branch document per worker.
2. **Branch workers** (parallel) — for each node: `trace_propose(parent: ...)`
   → write children (`parent`, `kind`, `order`, `condition`, acceptance
   criteria) → save.
3. **Reviewer** — `trace_lint` for every gap and tree rule (authoritative),
   plus `trace_tree(layer: "requirement", filter: {gaps_only: true})` to see
   uncovered/failing/suspect nodes in place; fix and repeat until lint is
   clean.

## 6. Inspecting the tree

```
handoff_trace_tree(root: "DSP-001")
handoff_trace_tree(scope: "DSP-", depth: 2)
handoff_trace_tree(layer: "requirement", filter: {gaps_only: true})
handoff_trace_tree(filter: {kind: ["select"]}, fields: ["own.state", "subtree.progress"])
```

CLI: `handoff-mcp trace tree --root DSP-001 --format text`.

Each node has `own` (its state / dev_stage / approval / coverage / suspect /
tasks) and `subtree` (leaf count, per-value counts, suspect count,
`progress.verified_ratio` / `implemented_ratio`). `gaps_only` keeps nodes
that have a defect — own state uncovered or failing, a suspect, a tree
finding, or a wiring gap (`orphan`, `dangling`, `invalid_link`, `cycle`,
`duplicate_id`) — plus their ancestors. `not_run` / `blocked` are work in
progress, and `unrefined` / `task_unlinked` are backlog; they are **not**
included (an `unverified` item already reads uncovered). `gaps_only` is a
defect view, not the lint gap list: read `trace_lint` for every gap.
Without `layer`, implicit `X#ACn` verification items (minimal/bugfix
profiles) are not listed as roots; pass `layer: "acceptance"` to see them.
Unknown arguments are rejected with an error naming them, so `gaps_only`
must go inside `filter`. Full parameter list: `handoff-trace` skill →
`references/tools.md`.

## 7. Things to expect

- A composite with no acceptance criteria of its own reports `unverified`
  (warning) only while none of its leaves is covered, and
  `missing_acceptance` (info). `waive-verify` on it stands while no child is
  covered; once the children cover it the waiver is unneeded and
  `redundant_waiver` (info) says so (remove the waiver). With many pure
  grouping nodes, lower those severities in `[trace.lint.rules]` if they are
  noise.
- After editing a document file directly, run `handoff_trace_report()` before
  `trace_propose`, which reads synced items only.
- Moving or deleting a parent never makes children suspect (decomposition is
  not a hashed link); `trace_impact` lists them as `tree_children_affected`,
  and `doc_save` lists children that lost their only parent as
  `orphaned_children`.
