# Pre-flight checklists (gates between requirement phases)

Check each gate before moving into the next phase of a requirements workflow.
A gate is a set of checks that can be answered from the project state, mostly
with one tool call each. A gate that fails sends you back to fix the earlier
phase; carrying a known gap forward only makes it more expensive.

The six gates assume this phase order:

| Phase | Purpose |
|---|---|
| 0 | Initialization: the V-model is configured and the project's purpose is stated |
| 1 | Architecture: top-level requirements, scopes, context, main use cases |
| 2 | Shared contracts: the non-functional requirements every feature must respect |
| 3 | Basic requirements: every scope decomposed to leaf requirements with acceptance criteria |
| 4 | Detailed requirements and verification design: specification and test layers |
| 5 | Roadmap: tasks, schedule, baseline, approval |

Tool parameters below are the ones the tools accept; the full reference is in
the `handoff-trace` skill (`references/tools.md`). Tick items in the
conversation or in a task note; nothing here is recorded by a tool.

## Gate 0 to 1: initialization complete

**Deliverables:** a configured trace profile and a first requirement-layer document.
**Quality bar:** the project can be summarized in one sentence and its actors are known.

- [ ] A trace profile is configured. `handoff_get_config` shows `[trace] profile` (and `layers` if customized). If not, follow the first-time setup in the `handoff-trace` skill.
- [ ] A requirement-layer document exists. `handoff_doc_list` and look for a document whose `layer` is `requirement`; if none, create one with `handoff_doc_save(slug=..., title=..., layer="requirement", body=...)`.
- [ ] The purpose and scope of the project can be stated in one sentence (write it at the top of the requirement document).
- [ ] The main stakeholders and actors are listed (a plain table in the document).

## Gate 1 to 2: architecture design complete

**Deliverables:** top-level requirements, scope split, context diagram, main use cases.
**Quality bar:** every scope has an owner prefix and the structure lints clean.

- [ ] Top-level requirements exist for each major capability. `handoff_trace_tree(layer="requirement", depth=1)` lists the roots.
- [ ] The work is split into scopes, each with a registered ID prefix (`[trace.id_prefixes]`, see `SKILL.md` section 1) and one root requirement. `handoff_trace_tree(scope="<PREFIX>-")` shows each branch.
- [ ] A system-context diagram is in the document (Mermaid or PlantUML in a fenced block is fine) showing actors and external systems.
- [ ] The main use cases are written with the use-case template in [templates.md](templates.md), kept in a plain document (not a layer document).
- [ ] `handoff_trace_lint` reports `counts.error = 0`.

## Gate 2 to 3: shared contracts complete

**Deliverables:** NFR items covering the applicable quality characteristics.
**Quality bar:** every NFR is measurable and has a verification method.

- [ ] The NFR checklist ([nfr-checklist.md](nfr-checklist.md)) was walked for all eight characteristics, each with NFRs or a recorded "not applicable" reason.
- [ ] An NFR item exists for every applicable concern. `handoff_trace_tree(scope="NFR-")` lists them.
- [ ] Cross-cutting quality targets (response time, availability, capacity, security level) have numeric values, units and conditions.
- [ ] Each NFR has `- method: manual` or `- method: auto` and an acceptance criterion. `handoff_trace_lint(rules=["missing_acceptance","ambiguous_word"])` is quiet for the NFR items.

## Gate 3 to 4: basic requirements complete

**Deliverables:** every scope decomposed to leaf requirements, each with acceptance criteria.
**Quality bar:** no structural gaps, each leaf is INVEST-checked and passes the quality review.

- [ ] Every scope is decomposed into features and leaves. `handoff_trace_tree(gaps_only=true)` returns no node with a tree-structure finding.
- [ ] Each leaf passed the INVEST check in [requirement-tree.md](requirement-tree.md).
- [ ] Every leaf has acceptance criteria (Given/When/Then or EARS). `handoff_trace_lint(rules=["missing_acceptance","leaf_without_acceptance"])` returns no findings.
- [ ] `handoff_trace_lint` has `counts.error = 0` and every warning was read and either fixed or consciously accepted.
- [ ] `handoff_trace_lint(action="quality_prompt")` was run and each requirement passed the quality aspects (the tool returns prompts; send them to an LLM or review by hand, and rewrite any item that fails with `handoff_trace_update` `upsert_item`).

## Gate 4 to 5: detailed requirements and verification design complete

**Deliverables:** specification-layer documents and verification items for every requirement.
**Quality bar:** coverage is complete on the horizontal axis and non-zero on the vertical axis.

- [ ] A basic-specification document exists. `handoff_doc_list` and look for a document whose `layer` is `basic_spec`.
- [ ] Verification items exist for the acceptance and system-test layers. Generate them with `handoff_trace_scaffold(items=[...], target_doc="<slug>", mode="preview")`, review, then `mode="apply"`.
- [ ] Horizontal coverage (requirement to its verification) is 100%. `handoff_trace_report` shows `coverage` per layer; list the rest with `handoff_trace_next(kinds=["write_verification"])`.
- [ ] Vertical coverage (requirement refined by the next layer down) is above 0% for every requirement that is not marked not-applicable. `handoff_trace_next(kinds=["refine"])` lists the remainder.
- [ ] A reviewer (a person or a review agent) has read the documents and passed them.

## Gate 5: roadmap complete

**Deliverables:** plan document, tasks linked to requirements, schedule, baseline, approval.
**Quality bar:** every requirement that needs implementation has a task, and the baseline is taken.

- [ ] A plan document exists. `handoff_doc_list(doc_type="plan")`.
- [ ] Tasks exist for every requirement that needs implementation. `handoff_trace_tasks` proposes missing ones (preview first) and `handoff_list_tasks` shows what is filed.
- [ ] The schedule was computed. `handoff_auto_schedule` was run (use `dry_run=true` first to check) after the last task change.
- [ ] A baseline snapshot was taken. `handoff_trace_baseline(action="create", tag="<release or plan name>")`, then `handoff_trace_baseline(action="list", limit=1)` to confirm.
- [ ] The user has approved the plan (record the approval in the plan document or the session decisions).
