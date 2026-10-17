# Layer document templates

Copy-paste starting points, one per layer. Every template follows the strict
**heading → attribute block → body → acceptance criteria** order. Save each
with:

```
handoff_doc_save(slug: "<feature>-<kind>", title: "...", layer: "<layer id>", body: "<markdown>")
```

`layer` is what turns the document into a layer document. Slugs allow only
`[a-z0-9-]` — write `auth-system-tests`, not `auth-system_test`. Replace `<...>`
placeholders; keep IDs project-unique.

**`test:` values are test names, not file paths.** Write the test's path as
the runner reports it (`auth::locks_after_five_failures`, or just
`locks_after_five_failures`). `trace_ingest` matches it exactly or as a
`::`-separated suffix of the reported name — nextest JUnit reports
`<crate>::<binary>::<fn>`, libtest JSON reports `<module>::<fn>`. A file path
such as `tests/auth.rs::...` never matches.

## Requirement (`layer: "requirement"`, left, level 1, paired with `acceptance`)

```markdown
# <Feature> Requirements

### FR-001 <short requirement title>

- priority: P1
- rationale: <why this requirement exists>

<One or two sentences stating what is required, not how.>

受入基準:
- AC1: Given <precondition> When <action> Then <observable outcome>
- AC2: WHEN <trigger> THE SYSTEM SHALL <response>
```

Non-functional requirements use the same shape with an `NFR-` prefix:

```markdown
### NFR-001 <quality attribute title>

- priority: P1

<Measurable statement: "95th-percentile response time under 200 ms at 50 rps".>

受入基準:
- AC1: Given <load profile> When <measurement> Then <threshold holds>
```

## Basic spec (`layer: "basic_spec"`, left, level 2, paired with `system_test`)

```markdown
# <Feature> Basic Spec

### SPEC-001 <short spec item title>

- refines: FR-001
- priority: P1

<How the requirement is realized at the external-behaviour / interface level.>
```

## Detailed spec (`layer: "detailed_spec"`, left, level 3, paired with `unit_test`)

```markdown
# <Feature> Detailed Spec

### DS-001 <short design item title>

- refines: SPEC-001

<Modules, data structures, algorithms, edge cases at implementation granularity.>
```

## Acceptance (`layer: "acceptance"`, right, level 1, verifies `requirement`)

```markdown
# <Feature> Acceptance Tests

### AT-001 <short check title>

- verifies: FR-001
- method: manual
- assignee: <assignee key>   (optional — must exist in config.toml [assignees], else a warning)

Steps: <steps to perform>.
Expected: <expected outcome>.
```

To cover a single acceptance criterion only: `- verifies: FR-001#AC2`.

## System test (`layer: "system_test"`, right, level 2, verifies `basic_spec`)

```markdown
# <Feature> System Tests

### ST-001 <short check title>

- verifies: SPEC-001
- method: auto
- test: <test_binary>::<test_fn>

<What the automated test asserts.>
```

## Unit test (`layer: "unit_test"`, right, level 3, verifies `detailed_spec`)

```markdown
# <Feature> Unit Tests

### UT-001 <short check title>

- verifies: DS-001
- method: auto
- test: <module>::tests::<test_fn>

<What the unit test asserts.>
```

## Smallest possible setups

**Minimal profile, one document.** Under `[trace] profile = "minimal"` each
acceptance criterion becomes its own verification item (`FR-001#AC1`), so the
requirement document alone closes the V:

```markdown
# <Feature> Requirements

### FR-001 <title>

- priority: P1

<statement>

受入基準:
- AC1: Given … When … Then …
```

**Inline verification (any profile).** A left item carrying its own
`method`/`test` is its own verifier — no second document:

```markdown
### SPEC-010 <title>

- refines: FR-001
- method: auto
- test: api::rejects_expired_token

<statement>
```

**Bug fix.** Under `profile = "bugfix"` (or a per-document
`doc_save(trace_profile: "bugfix")`), the requirement layer is displayed as
再現条件 (reproduction) and acceptance as 回帰テスト (regression test):

```markdown
# <Bug> Reproduction and regression test

### FR-900 <short reproduction title>

- priority: P1

<Reproduction steps as the requirement statement.>

受入基準:
- AC1: <the specific regression check that must now pass>
```

## Mixing a definition item and its verifier in one document

`- layer:` overrides the document's layer for one item:

```markdown
# <Feature> Requirements and Acceptance

### FR-020 <title>

- priority: P2

<statement>

### AT-020 <check title>

- layer: acceptance
- verifies: FR-020
- method: manual

<steps / expected>
```

## Requirement tree (scoped prefixes)

Register the scope prefixes first:

```toml
[trace.id_prefixes]
requirement = ["ARCH", "SYS", "DSP"]
```

```markdown
# Display branch

### SYS-001 Stream deck controller

- kind: group

The device shows and triggers actions on its keys.

### DSP-001 Key display

- parent: SYS-001
- kind: group

Keys show the state of their assigned action.

### DSP-001-001 Redraw on state change

- parent: DSP-001
- order: 1

When an action's state changes, only the keys bound to it are redrawn.

受入基準:
- AC1: Given key 3 is bound to timer.toggle When timer.running changes Then only key 3 is redrawn
```

See [requirement-tree.md](requirement-tree.md) for decision/select branches.
