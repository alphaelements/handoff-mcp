---
name: brainstorm-execute
description: Execute brainstorm research phase — parallel tech investigation + regulatory research + adversarial review.
---

# brainstorm-execute — Phase 2 research workflow

Reference for the **brainstorm coordinator** (`/brainstorm-loop`). The coordinator calls
`Skill(handoff-task-loop:brainstorm-execute)` once the user has approved research (or, in delegate
mode, once the coverage is filled). This skill tells you how to turn the dialogue state into
workflow `args`, launch the workflow, validate what comes back, and file it in the report.

This skill does not talk to the user about research design — the coordinator already did. It runs
the research and returns structured results.

## 1. What the workflow does

```
Stream A — technical (research-investigator x3, parallel)
  feasibility | existing solutions | competitors and similar cases

Stream B — regulatory (two tiers; runs in parallel with Stream A)
  brainstorm-regulatory-director      scopes the areas that apply to THIS product
  -> brainstorm-regulatory-investigator x N   one per area, parallel
  -> brainstorm-regulatory-director           integrates and verifies

After A and B finish:
  brainstorm-adversarial-reviewer     reads everything, looks for reasons to stop or reshape
```

Every agent is read-only and returns schema-validated JSON (plus a markdown `report`). The
workflow itself writes nothing to handoff — **you** file the results (section 5).

## 2. Build the args

Read the session first:

```
handoff_brainstorm_status(brainstorm_id)   -> decisions, open_questions, topics, report_doc_id
handoff_memory_query(...)                  -> user skills / equipment / constraints (if not already in hand)
```

Then write `brainstorm_summary`: a self-contained plain-text brief (vision, motivation, target users,
core value, scope in/out so far, tech and business constraints, the user's skills and equipment,
open questions). **The agents see nothing else** — no dialogue history, no conversation. Missing
context here becomes shallow or generic research.

If the status response carries `project_type` or `target_milestones`, put them in the summary too.
For a `gui-*` project, include what was settled under `gui-verification-strategy` (target platform,
how the GUI can be run and captured) so the research covers GUI verification options for that
platform; for `target_milestones`, name them so findings can be tied to the milestone they affect
(the coordinator files the per-milestone scope table at finalize, report `### 5.5`).

| arg | required | default | meaning |
|---|---|---|---|
| `brainstorm_id` | yes | | from `handoff_brainstorm_start` |
| `topic` | yes | | brainstorm theme |
| `brainstorm_summary` | yes | | brief described above |
| `response_language` | no | `English` | set to the user's language |
| `additional_instructions` | no | | extra instructions for every agent |
| `tech_investigations` | no | feasibility / existing / competitors | `[{id, title, questions[], sources_hint?}]`; replaces the three defaults. Ids must be unique |
| `skip_regulatory` | no | `false` | `true` = no Stream B. Only when nothing about the product is regulated; the reviewer will list regulation as unexamined |
| `max_regulatory_investigators` | no | `6` | cap on regulatory areas investigated in parallel |
| `investigator_model` | no | `sonnet` | research-investigator and regulatory investigators |
| `director_model` | no | `opus` | regulatory director |
| `reviewer_model` | no | `opus` | adversarial reviewer |

### Turn attachments (GUI projects)

During Phase 1 the coordinator records wireframes, mockups and screenshots the user referred to
through the `attachments` parameter of `handoff_brainstorm_turn`:

```
handoff_brainstorm_turn(brainstorm_id, topic_tag="wireframe-review", summary=..., decisions=[...],
  attachments=[
    { path: 'wireframes/login.png', kind: 'wireframe', screen: 'login' },
    { path: 'mocks/dashboard.png', kind: 'mockup', screen: 'dashboard' },
    { path: 'screenshots/current.png', kind: 'screenshot' }
  ])
```

- `path` (required, non-empty): file path or link to the image.
- `kind` (required): `wireframe` | `mockup` | `screenshot`. Any other value is rejected.
- `screen` (optional): screen name; at finalize it is matched (trimmed, case-insensitive) against the
  `screens` list. Matched attachments appear in that screen's wireframe-reference cell of report 8.1;
  unmatched or screen-less ones are listed in 8.4.
- Attachments accumulate across turns (exact duplicates are dropped); `handoff_brainstorm_status`
  reports `attachments_count`.

Use them as visual context for Phase 2: the research agents see only `brainstorm_summary`, so when
the session has attachments, list each one there (path, kind, screen, and what the user said about
it) so the research can take the intended look and flow into account.

Tailor `tech_investigations` questions to the topic when the defaults would be too generic (for
example a hardware product: add a facet on the specific sensor or radio the idea depends on), but
keep the three default ids when you keep their meaning — the filing table in section 5 maps them
to report sections.

## 3. Launch the workflow

**Always use `name: 'handoff-task-loop:brainstorm-execute'`. Never write an inline script** — inline
scripts bypass the agent definitions (investigators are Sonnet, director and reviewer are Opus).
All customization goes through `args`.

> **Resuming a run**: `resumeFromRunId` does NOT auto-inherit `args`. Pass the same `args` again.

```javascript
Workflow({
  name: 'handoff-task-loop:brainstorm-execute',
  args: {
    brainstorm_id: '<id from handoff_brainstorm_start>',
    topic: '<brainstorm theme>',
    brainstorm_summary: '<self-contained brief, see section 2>',
    response_language: '<user language>',
    // optional:
    // tech_investigations: [{ id: 'feasibility', title: '...', questions: ['...'], sources_hint: '...' }],
    // skip_regulatory: false,
    // max_regulatory_investigators: 6,
    // investigator_model: 'sonnet', director_model: 'opus', reviewer_model: 'opus',
  },
});
```

Put the Workflow run id in the summary of the brainstorm turn that moved the phase to `research`
(and in the session notes), so an interrupted session can resume.

## 4. Read the result

All fields are always present.

| field | notes |
|---|---|
| `status` | `complete` = both streams complete and review obtained. `partial` = something is missing (see `warnings`). `failed` = no stream produced a usable result |
| `stream_a.status` | `complete` / `partial` / `failed` |
| `stream_a.results[]` | `{facet_id, summary, findings[{claim, evidence, source, confidence}], gaps[], recommendation?, report?}` |
| `stream_a.failed_facets[]` | technical facets that returned nothing usable |
| `stream_b.status` | `complete` / `partial` / `failed` / `skipped` |
| `stream_b.scope` | director's `{rationale, areas[{id, title, why, questions[], jurisdictions?, trigger?, priority?}], out_of_scope?}` |
| `stream_b.investigations[]` | per-area `{area_id, summary, findings[], gaps[], risk_level, needs_professional_review?}` |
| `stream_b.synthesis` | director's integration `{status, consolidated[{area_id, conclusion, risk_level, requires_action, action?}], contradictions[], unverified_areas[], needs_professional_review?}`; `null` when the director's integration failed (investigations are then **unverified**) |
| `stream_b.failed_areas[]` / `deferred_areas[]` | areas whose investigator failed / areas dropped by the cap |
| `review` | `{verdict, risks[{severity, axis, description, evidence, mitigation, decision_needed_from_user?}], showstoppers[], unexamined[], cross_stream_conflicts?, conditions?}` or `null` |
| `review.verdict` | `PROCEED`, `PROCEED_WITH_CONDITIONS`, or `RECONSIDER` |
| `warnings[]` | every degradation, in plain words |
| `research_log[]` | `{stream, step, status}` trail |

**Fail closed.** A missing or malformed agent result is reported as a failure, never as a pass:

- `review: null` means the research is **unreviewed** — say so; do not present the project as vetted.
- `stream_b.status` of `failed` or `partial` means regulatory exposure is not established — do not
  tell the user "no regulatory issues". Say which areas are unverified.
- On `partial` or `failed` you may re-run the workflow once with narrower `tech_investigations`
  (only the failed facets), or continue with the gap stated openly. Ask the user in Phase 3 when the
  gap matters to a decision. Never fabricate the missing findings.

## 5. File the results in the report document

The report document (`report_doc_id` from `handoff_brainstorm_status`) has a research block that
`handoff_brainstorm_finalize` preserves:

```
## 6. 調査結果
### 6.1 技術的実現可能性
### 6.2 既存ソリューション
### 6.3 競合分析
### 6.4 法務・規制
```

1. `handoff_doc_get(doc_id=report_doc_id)` to find each section's `seq`.
2. `handoff_doc_update_section(doc_id, seq, new_content)` for each section. `new_content` includes the
   section's own heading line.

| section | content |
|---|---|
| 6.1 | Stream A `feasibility` result: summary, findings with sources, gaps, recommendation. Any custom facet becomes a `####` sub-heading here |
| 6.2 | Stream A `existing` result |
| 6.3 | Stream A `competitors` result |
| 6.4 | Stream B: scope and rationale, the director's consolidated conclusions per area with risk level, contradictions, unverified areas, items needing professional review. If `synthesis` is `null`, write the raw investigation summaries and label them **unverified** |

Keep the evidence trail: every claim keeps its source; confidence ratings stay visible; gaps are
written down, not smoothed over. Regulatory text is **not legal advice** — keep the "needs
professional review" items.

For a `gui-*` project the report also has `## 8. GUI 設計概要` (8.1 screen table: name | function |
priority | wireframe ref; 8.2 screen flow; 8.3 UX policy). It is not a research section: the
coordinator fills it at finalize from `screens`, `screen_flow` and `ux_policy`. Do not write it here;
if research findings change the screens (a platform limit that removes one), hand that back in your
brief so the coordinator reflects it in those arguments.

Risks from `review.risks` are filed through the dialogue log, not by editing section 7 directly
(finalize renders section 7 from recorded decisions): in Phase 3, record each accepted or open risk
with `handoff_brainstorm_turn(topic_tag="risks", decisions=[...], open_questions=[...])`.

## 6. Hand back to the coordinator

Return a short brief — not the raw JSON — covering:

1. Run status and every entry of `warnings`.
2. Per stream: the 2-4 findings that change a decision, in plain language.
3. The reviewer's verdict, BLOCKER risks first, then MAJOR; `showstoppers` verbatim in spirit;
   `unexamined` items; which risks need a user decision (`decision_needed_from_user`).
4. Which report sections (6.1-6.4) were filled and which are marked unverified or missing.

The coordinator presents this to the user in Phase 3.

## Rules

- Always `name: 'handoff-task-loop:brainstorm-execute'`; never an inline script.
- Never fabricate or soften findings; carry gaps and warnings through.
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only.
- Do not run the workflow without the coordinator's approval gate (user approval, or delegate mode).
- Do not push, commit, or touch project files.
