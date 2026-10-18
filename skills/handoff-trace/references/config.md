# `[trace]` configuration reference

Everything the V-model traceability features read from the project's
`.handoff/config.toml`. Tool signatures are in `tools.md`; how coverage and
verification are computed is in `verification.md`; suspect/baseline handling
is in `change-management.md`. How to write the items themselves is the
separate `handoff-requirements` skill.

## 1. Where it lives, and the default state

All settings live under `[trace]` (and its sub-tables) in
`.handoff/config.toml`.

`handoff_init` writes **no `[trace]` section at all**. With nothing
configured, the effective behavior is:

| aspect | effective value with no `[trace]` |
|---|---|
| used layers | auto-detected: every registered layer that has at least one item |
| project profile | none (so no implicit acceptance items, no `default_needs` gating) |
| `done_guard` | `warn` |
| `auto_layer` | `false` (documents only become layer documents when you pass `layer=`) |
| lint | every built-in rule at its default severity, no policy rules |

Nothing is "switched off", but very little happens until documents carry a
`layer`. When the used-layer set comes out empty, `trace_report` and similar
tools say so in `warnings` (codes `DIAG-T001`/`DIAG-T002`) instead of
returning a silently empty result.

### Turning the V-model on

Smallest useful setup: pick a profile, and optionally let `doc_save` infer
the layer from `doc_type`.

```toml
[trace]
profile = "standard"   # minimal | standard | full | bugfix | <your own>
auto_layer = true      # spec -> requirement, design/plan -> detailed_spec
```

`design`/`plan` documents are inferred as `detailed_spec`, which is not in
`minimal` or `standard`: their items then drop out as `layer_outside_profile`
and their parents stay `unrefined`. Under those profiles pass
`layer: "basic_spec"` explicitly for design documents, or set
`plan_default_layer`.

The same through the MCP tool (or `handoff-mcp config update --updates '{...}'`):

```
handoff_update_config(updates: {
  "trace.profile": "standard",
  "trace.auto_layer": true
})
```

`handoff_update_config` writes any dotted key up to **4 segments** deep
(`trace.lint.rules.unverified`). Values are converted from JSON: a string,
number, or bool becomes the same TOML scalar, an array becomes a TOML array,
and an object becomes an **inline table**. `null` is written as an empty
string. Values are written as given, without validation. Working examples:

```
handoff_update_config(updates: {
  "trace.layers": ["requirement", "basic_spec", "acceptance", "system_test"],
  "trace.done_guard": "block",
  "trace.plan_default_layer": "basic_spec",
  "trace.id_prefixes.requirement": ["UC"],
  "trace.lint.rules.unverified": "error",
  "trace.profiles.web": {"extends": "standard", "max_generated_per_call": 20},
  "trace.lint.require": [
    {"id": "p0-verified", "when": {"layer": "requirement", "priority": ["P0"]}, "need": "verified_by"}
  ]
})
```

Caveats:

- An array-valued key (`trace.lint.require`, `trace.layer`) is **replaced
  as a whole**: you cannot append one entry. Send the full list.
- An object becomes one inline table. Nested tables that are 5 or more keys
  deep cannot be written key by key, so pass them as an object at a
  shallower key instead (as with `trace.profiles.web` above).
- Updating one of the typed keys (`settings.*`, `dashboard.*`,
  `project.name`/`project.description`) rewrites the whole file through the
  config model. `[trace]` survives, but comments and hand formatting are
  lost and inline tables are expanded into regular tables.
- `[[trace.layer]]` declarations and long policy lists are easier to keep
  readable if you edit `config.toml` by hand.

`handoff_get_config` returns the whole file as JSON, `[trace]` included.

## 2. Which layers are "in use"

The project-wide used-layer set is resolved in this order. The first one that
yields a non-empty list wins:

1. `[trace] layers`: an explicit list.
2. The project default profile's `layers` (`[trace] profile`).
3. Auto-detection: every registered layer that has at least one item.

If both `layers` and `profile` are set, `layers` wins and every trace tool
reports `[trace] layers and [trace] profile = "<name>" are both set; [trace]
layers takes priority` in `warnings`. The profile's other effects
(`implicit_acceptance`, `default_needs`, `max_generated_per_call`) still
apply.

A document's own `trace_profile` (set with `doc_save(trace_profile=...)`)
overrides this for the requirement tree reachable from that document's
items (the stricter union applies where trees meet). See the `handoff-trace`
SKILL.md "Profiles in one paragraph".

An item whose layer is not in its effective used-layer set drops out of
coverage and state (lint `layer_outside_profile`, info).

## 3. Key reference

| key | type | default | effect | example |
|---|---|---|---|---|
| `[trace] layers` | array of layer ids | `[]` (auto) | explicit used-layer set (highest priority, §2) | `layers = ["requirement", "acceptance"]` |
| `[trace.id_prefixes]` `<layer> = [...]` | table of string arrays | `{}` | extra heading-ID prefixes for a layer. **Added to** the defaults, never replaces them | `requirement = ["UC", "SYS"]` |
| `[[trace.layer]]` | array of tables | none | declares a custom layer (§4.2) | see §4.2 |
| `[trace] profile` | string | unset (auto) | project default profile: a built-in name or a `[trace.profiles.<name>]` key | `profile = "standard"` |
| `[trace] done_guard` | `"warn"` \| `"block"` \| `"off"` | `"warn"` | what happens when a task with outstanding verification moves to review/done (§5) | `done_guard = "block"` |
| `[trace.profiles.<name>]` | table | none | custom profile (§4.4) | `[trace.profiles.web]` |
| `[trace.lint.rules]` `<rule> = "<sev>"` | table of strings | `{}` | per-rule severity override (§7) | `unverified = "error"` |
| `[[trace.lint.require]]` | array of tables | none | project policy rules (§7) | see §7 |
| `[trace] auto_layer` | bool | `false` | `doc_save` without a `layer` argument infers one from `doc_type` (§6) | `auto_layer = true` |
| `[trace] plan_default_layer` | string | `"detailed_spec"` | the layer `auto_layer` gives `plan` documents | `plan_default_layer = "basic_spec"` |

`[[trace.layer]]` entry:

| key | type | required | meaning |
|---|---|---|---|
| `id` | string | yes | layer id, used in `doc_save(layer=...)`, `- layer:` and every config key |
| `display_name` | string | no | name shown in reports and UI. Defaults to `id` |
| `side` | `"left"` \| `"right"` | yes | left = definition, right = verification |
| `level` | integer ≥ 1 | yes | depth within its side (1 = top) |
| `pair` | layer id | yes | the opposite-side layer it verifies or is verified by. Must point back |
| `id_prefixes` | string array | no (but needed to write items) | default heading-ID prefixes |

`[trace.profiles.<name>]` entry:

| key | type | default | meaning |
|---|---|---|---|
| `extends` | profile name | none | inherit unset fields from a built-in or another custom profile |
| `layers` | layer-id array | inherited | this profile's used layers. Empty/omitted = inherit |
| `implicit_acceptance` | bool | inherited, else `false` | turn each acceptance criterion into its own verification item |
| `max_generated_per_call` | integer | none | warning threshold for generators (§4.6) |
| `default_needs` | table `<layer> = [layer, ...]` | derived from `layers` | coverage required per layer (§4.5) |

`[[trace.lint.require]]` entry:

| key | type | default | meaning |
|---|---|---|---|
| `id` | string | required | rule id that appears in `findings[].rule` |
| `when.layer` | layer id | any | only items on this layer |
| `when.priority` | string array | any | only items whose `priority` is one of these (`P0`…`P3`). Items without a priority never match |
| `when.method` | string | any | only items whose own `method` equals this (`manual`/`auto`/`visual`/`review`) |
| `when.doc` | document slug | any | only items owned by this document |
| `when.approval` | string or string array | any | only items whose approval is one of these (`draft`/`review`/`approved`) |
| `need` | string | required | what must hold (vocabulary in §7) |
| `severity` | `"error"` \| `"warning"` \| `"info"` | `"error"` | finding severity |

## 4. Layers and profiles

### 4.1 Built-in layers

| id | display name | side | level | pair | default prefixes |
|---|---|---|---|---|---|
| `requirement` | 要件 | left | 1 | `acceptance` | `REQ`, `FR`, `NFR` |
| `basic_spec` | 基本仕様 | left | 2 | `system_test` | `SPEC`, `BS` |
| `detailed_spec` | 詳細仕様 | left | 3 | `unit_test` | `DS` |
| `acceptance` | 受入検証 | right | 1 | `requirement` | `AT`, `AC` |
| `system_test` | システムテスト | right | 2 | `basic_spec` | `ST` |
| `unit_test` | ユニットテスト | right | 3 | `detailed_spec` | `UT` |

The built-ins cannot be redefined or removed. To use fewer of them, choose a
profile or set `layers`.

### 4.2 Custom layers

```toml
[[trace.layer]]
id = "ux_spec"
display_name = "UX spec"
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

A declaration is **disabled with a warning** if any of the following is true.
The rest of the config keeps working:

- `id` is empty, or duplicates an existing layer (built-in or an earlier custom one)
- `side` is not `"left"` or `"right"`
- `level` is below 1 or above 255
- `pair` names no known layer, the paired layer's `pair` does not point
  back, or both are on the same side
- one of its `id_prefixes` collides with another accepted layer's prefixes
- its partner was disabled for any of these reasons (the pair is dropped together)

The warnings look like `trace.layer 'ux_spec': pair "usability_test" does not
point back (not reciprocal), disabled`. They appear in the `warnings` of
`trace_report`, `trace_lint`, and the other trace tools.

### 4.3 Built-in profiles

| profile | layers | implicit_acceptance | display-name overrides |
|---|---|---|---|
| `minimal` | requirement, acceptance | `true` | none |
| `standard` | requirement, basic_spec, acceptance, system_test | `false` | none |
| `full` | all 6 built-ins | `false` | none |
| `bugfix` | requirement, acceptance | `true` | requirement → 再現条件, acceptance → 回帰テスト |

Built-in profiles set no `max_generated_per_call` and no lint behavior of
their own. Their `default_needs` is always derived from `layers` (§4.5).
`bugfix` differs from `minimal` only in its display names. Those names show
up where a document uses `trace_profile = "bugfix"` (`trace_report`'s
`profile.overrides[].display_names`). The layer ids do not change.

### 4.4 Custom profiles and `extends`

```toml
[trace]
profile = "web"

[trace.profiles.web]
extends = "standard"
layers = ["requirement", "ux_spec", "acceptance", "usability_test"]
implicit_acceptance = false
max_generated_per_call = 20

[trace.profiles.web.default_needs]
requirement = ["acceptance"]
ux_spec = ["usability_test"]
```

Resolution walks the `extends` chain starting at the named profile. For each
field (`layers`, `implicit_acceptance`, `max_generated_per_call`,
`default_needs`) the **first profile in the chain that sets it wins**. A
built-in ends the chain. These problems produce a warning and stop the walk,
keeping whatever was resolved so far:

- the chain loops back on itself (a cycle)
- the chain is more than 8 profiles deep
- `extends` names an unknown profile

Layer ids the registry does not know are dropped with a warning. If nothing
in the chain sets `layers`, the profile does not resolve, and the project
falls back to auto-detection.

A `[trace.profiles.<name>]` whose name matches a built-in (`minimal` etc.)
is ignored, because the built-in always wins. Custom profiles cannot define
display-name overrides.

### 4.5 `default_needs` and per-item `needs`

`needs` decides **which layers count** toward a left-side item's coverage.
It applies to both axes: verifiers for horizontal coverage, refining
children for vertical coverage. It is resolved per item:

| item's `- needs:` line | effect |
|---|---|
| absent | fall back to the **project default profile's** `default_needs` entry for the item's layer |
| present, empty (`- needs:`) | no coverage required. The axis reports `waived` |
| present, listed (`- needs: acceptance, system_test`) | only verifiers or children on those layers count |

With the fallback:

- **No project profile** (`[trace] profile` unset): items are not gated.
  Every in-scope verifier counts, which is the classic behavior.
- **Profile set, but no explicit `default_needs`**: it is derived from the
  profile's `layers`. Each left-side layer needs its `pair` (if that pair is
  in `layers`) plus the next-deeper left-side layer (if one is in `layers`).
  For `standard` the result is `requirement = ["acceptance", "basic_spec"]`
  and `basic_spec = ["system_test"]`. Right-side layers get no entry.
- **Explicit `[trace.profiles.<name>.default_needs]`**: used as written.
  A layer with no entry there is not gated.

A verifier on a layer outside the item's needs is excluded from coverage and
reported by lint `unwanted_coverage` (info). Unknown layer ids in an item's
`needs:` are ignored with a warning. Only the project default profile
supplies `default_needs`. A document's `trace_profile` override does not
change it.

### 4.6 `max_generated_per_call`

This is a safety net, not a limit. When `trace_scaffold` or `trace_tasks`
in `mode="apply"` generates more items than this value, the response
carries the warning `Generated N items, exceeding profile limit of M`.
Nothing is truncated or undone, because the tool's own `limit` argument
still decides how much is generated. `trace_scaffold` uses the target
document's `trace_profile` if it has one, otherwise the project profile.
`trace_tasks` always uses the project profile.

## 5. `done_guard`

Applies when `handoff_update_task` moves a task to
`review` or `done` while the task has a requirement link with a blocker.
A blocker is a linked verifier that is not run, failing, blocked, or needs
re-verification, a stale task-link suspect, or a linked item whose approval
is still `draft` (`approval_draft`; `review` is not a blocker). The full
list and how to clear each: `verification.md` → done guard.

| value | behavior |
|---|---|
| `warn` (default) | the transition applies and the blockers are appended to the response text (`N outstanding blocker(s) …`) |
| `block` | the call is rejected unless `force: true` |
| `off` | no check |

Any other value is treated as `warn`. Tasks without requirement links are
never affected. `handoff_bulk_update_tasks` applies the same guard to every
update that moves a task into `review`/`done`: in `block` mode the update is
rejected into `errors[]` (the others still apply) unless the call carries
`force: true`; in `warn` mode it is applied and a `warnings[]` entry
(`{task_id, warning}`) is returned. Edits made outside the MCP tools — a UI
that writes task files directly — still bypass the guard.

## 6. `auto_layer` and `plan_default_layer`

With `auto_layer = true`, `doc_save` infers a layer from `doc_type` only
when **no `layer` argument is passed** and the document has no layer yet.
An explicit `layer`, including `""` to clear it, always wins — but only for
that call: a cleared document is inferred again by the next save that omits
`layer`, so keep passing `layer: ""` (or change its `doc_type`). The response
tells you about the inference: `Layer auto-inferred as '<layer>' from
doc_type '<type>' ...`.

| `doc_type` | inferred layer |
|---|---|
| `spec` | `requirement` |
| `design` | `detailed_spec` |
| `plan` | `plan_default_layer` (default `detailed_spec`; an empty value also means the default) |
| `adr`, `guide`, `note`, anything else | none |

The value of `plan_default_layer` is not validated. Make sure it names a
registered layer.

## 7. Lint configuration

Rule ids, categories, and defaults are listed in `lint-rules.md`.

### `[trace.lint.rules]`: severity overrides

```toml
[trace.lint.rules]
unverified = "error"          # escalate
unbaselined = "off"           # silence entirely
p0-verified = "warning"       # also works for a [[trace.lint.require]] id
```

Values: `"error"`, `"warning"`, `"info"`, `"off"`. If an override names a
rule id that is neither built-in nor a `require` id, or uses any other
value, `trace_lint` fails as a config error (CLI exit 2).

### `[[trace.lint.require]]`: policy rules

```toml
[[trace.lint.require]]
id = "approved-p0-needs-verifier"
when = { layer = "requirement", priority = ["P0", "P1"], approval = ["review", "approved"] }
need = "verified_by"
severity = "error"
```

Every item matching **all** the `when.*` keys you set (`when.layer`,
`when.priority`, `when.method`, `when.doc`, `when.approval`) must satisfy
`need`. Each item that does not satisfy it becomes a finding with
`rule = <id>`. Leaving out a `when.*` key means no filter on it, so a rule
with no `when` applies to every item. `when.approval` accepts a single string
(`approval = "approved"`) or an array.

`need` vocabulary:

| `need` | satisfied when the item |
|---|---|
| `verified_by` | has at least one valid verifier |
| `refined_by` | has at least one valid refining child |
| `implemented_by_task` | has a task linked to it with the `implements` role |
| `passing` | has aggregate state `passing` |
| `no_suspect` | has no suspect (link, task, or result) |
| `auto_test` | has `method: auto` itself, or at least one verifier with `method: auto` |

An entry with no `id`, a missing or unknown `need`, or an invalid `severity`
makes `trace_lint` fail as a config error (CLI exit 2). It is never
silently skipped.

## 8. Full annotated example

```toml
[trace]
# Used layers come from the profile. Uncomment to pin them explicitly
# (that wins over the profile, with a warning).
# layers = ["requirement", "basic_spec", "acceptance", "system_test"]
profile = "web"
done_guard = "block"                 # review/done needs passing verification (force: true overrides)
auto_layer = true                    # spec -> requirement, design/plan -> layer below
plan_default_layer = "basic_spec"

[trace.id_prefixes]
requirement = ["UC", "SYS"]          # added to REQ/FR/NFR
system_test = ["E2E"]                # added to ST

# A custom left/right pair. Both halves must point at each other.
[[trace.layer]]
id = "ux_spec"
display_name = "UX spec"
side = "left"
level = 2
pair = "usability_test"
id_prefixes = ["UX"]

[[trace.layer]]
id = "usability_test"
display_name = "Usability test"
side = "right"
level = 2
pair = "ux_spec"
id_prefixes = ["UXT"]

[trace.profiles.web]
extends = "standard"                 # inherits implicit_acceptance = false
layers = ["requirement", "basic_spec", "ux_spec", "acceptance", "system_test", "usability_test"]
max_generated_per_call = 25          # warn when scaffold/tasks generate more than this

[trace.profiles.web.default_needs]   # omit to derive from layers
requirement = ["acceptance", "basic_spec"]
basic_spec = ["system_test"]
ux_spec = ["usability_test"]

[trace.profiles.hotfix]
extends = "bugfix"                   # requirement + acceptance, implicit acceptance items

[trace.lint.rules]
unverified = "error"
unbaselined = "off"
passive_voice_hint = "off"

[[trace.lint.require]]
id = "p0-needs-verification"
when = { layer = "requirement", priority = ["P0"] }
need = "verified_by"
severity = "error"

[[trace.lint.require]]
id = "approved-auto-tested"
when = { layer = "basic_spec", approval = "approved" }
need = "auto_test"
severity = "warning"
```

## 9. When changes take effect

- Config is read on every tool call. There is no cache and no restart needed.
- Some settings change what a layer document parses into:
  - layer declarations (`[[trace.layer]]`)
  - `[trace.id_prefixes]`
  - `[trace] profile`
  - any profile's `implicit_acceptance`

  Changing one of these marks every layer document as out of sync. Each
  document is re-parsed the next time it is written (`doc_save`,
  `doc_update_section`, `doc_verify(sync)`) or when `trace_report`,
  `trace_slice`, or a writing `trace_suspect` action resyncs directly
  edited documents. Until then, read-only tools such as `trace_lint` resync
  in memory only and report each such document as `unsynced_body`. Run
  `trace_report` once after a change like this to persist the resync.
- `layers`, `done_guard`, `[trace.lint]`, `auto_layer`, and
  `plan_default_layer` do not change parsing. They take effect on the next
  call that reads them. `auto_layer` only affects later `doc_save` calls,
  never existing documents.
