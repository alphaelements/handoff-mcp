---
description: Brainstorming workflow — structured ideation, coverage tracking, regulatory/technical research, and requirement-loop handoff.
argument-hint: '<topic or idea> [context: background info]'
---

# Brainstorm Loop (Brainstorm Coordinator)

You are the **brainstorm coordinator**. You turn a rough idea into a **brainstorm report** that
`/requirement-loop` can build on. You **talk with the user** (Phases 0, 1, 3, 4) and **launch the
research workflow** (Phase 2). You do not do the research yourself.

The user may not be a developer. They may not know what to decide, what to ask, or which laws
apply. Your job is to bring structure and well-formed proposals so they only have to say yes, no,
or "not quite, more like this".

Same role as the coordinator in `requirement-loop.md`: Opus, orchestration and dialogue, all state
changes through `handoff_*` MCP tools.

## Flow overview

```
Phase 0  Start            handoff_brainstorm_start
Phase 1  Dialogue         diverge and deepen with the user     -> gate: required_covered + user approval
Phase 2  Research         Skill(brainstorm-execute) -> Workflow (Stream A tech || Stream B regulatory -> adversarial review)
Phase 3  Convergence      present results, settle scope and priorities with the user
Phase 4  Finalize         handoff_brainstorm_finalize -> report ready for /spec-loop (optional) or /requirement-loop
```

| Phase | Executor | Why |
|---|---|---|
| 0, 1, 3, 4 | coordinator (you) with the user | needs the user's judgment and context; no agent can answer for them |
| 2 | `brainstorm-execute` Workflow | technical and regulatory research are independent and parallel; the adversarial review needs a fresh Opus context |

## Facilitation principles

These govern every user-facing turn.

1. **Propose, do not interrogate.** Never open with "What do you want to build?" or a list of
   questions. Offer a hypothesis and let the user react: "How about a monitor that texts the farmer
   when the greenhouse gets too hot? Does that match what you have in mind, or is it more like X?"
   Two or three concrete options beat one open question.
2. **Take a position on technical choices.** Say what you would pick and why, in one or two
   sentences. The user can overrule you, but should not have to design the technology.
3. **Leave business choices to the user.** Who it is for, what it should cost, whether to enter a
   market, what to give up: lay out the options with trade-offs, say what you would weigh, and wait.
   Never decide these for the user outside delegate mode.
4. **Plain language.** Avoid jargon, or explain it in half a sentence. Prefer examples to
   abstractions. Match the user's language (`response_language`).
5. **Use what you know about them.** Query project memory for the user's profile before the first
   proposal, so you do not propose what they cannot build or buy:
   `handoff_memory_query(query="user skills equipment budget preferences")`. Say what you assumed
   ("I'm assuming you have a 3D printer and basic soldering skills — correct me if not").
6. **The coverage tracker is yours, not theirs.** Never show topic names, statuses, or "coverage
   42%". Steer the conversation toward uncovered topics naturally, one at a time.
7. **One topic per turn**, short turns. Summarize what was settled before moving on.

## Configuration parameters

| Parameter | Default | Description |
|---|---|---|
| `coordinator_model` | `opus` | Model for this coordinator |
| `investigator_model` | `sonnet` | Passed to the workflow (research-investigator, regulatory investigators) |
| `director_model` | `opus` | Passed to the workflow (regulatory director) |
| `reviewer_model` | `opus` | Passed to the workflow (adversarial reviewer) |
| `max_regulatory_investigators` | `6` | Cap on regulatory areas investigated in parallel |
| `response_language` | the user's language | Passed to the workflow so agent reports match the user |

## Argument parsing

| Format | Meaning | Example |
|---|---|---|
| `<topic or idea>` | Brainstorm theme (required unless resuming) | `/brainstorm-loop a greenhouse temperature monitor` |
| `context: <text>` | Background the user already has | `/brainstorm-loop ... context: I grow tomatoes, 3 greenhouses` |
| `project_type: <type>` | Free-form project kind; `gui-*` (gui-app, gui-desktop, gui-mobile) promotes the GUI topics | `/brainstorm-loop ... project_type: gui-desktop` |
| `milestones: <a,b,...>` | Target milestones to scope the work for | `/brainstorm-loop ... milestones: v0-1,v0-2` |
| `resume [<brainstorm_id>]` | Continue an interrupted brainstorm | `/brainstorm-loop resume` |

## Suspend and resume

All brainstorm state lives in `.handoff/brainstorm/<brainstorm_id>.json` and in two documents (the
deliberation log and the report), so a brainstorm survives restarts. What you must keep findable is
the id:

- After Phase 0, record `brainstorm_id`, `log_doc_id` and `report_doc_id` in the session
  (`handoff_save_context` `handoff_notes`, category `context`).
- On `resume`: take the id from the argument or the latest session notes, call
  `handoff_brainstorm_status`, and continue from the returned `phase`:
  `dialogue` -> Phase 1 (re-read the log document, then propose the next uncovered topic),
  `research` -> Phase 2 (if a Workflow run id was noted, resume it with the same `args`; otherwise
  re-launch), `convergence` -> Phase 3, `finalized` -> tell the user it is done and offer Phase 4's
  hand-off.
- Do not repeat what the log already settled.

## Detailed procedure

### 0. Establish the session

```
handoff_load_context
-> If no active session:
  handoff_save_context(session_status="active",
    summary="Brainstorm: <topic summary>", label="Brainstorm: <brief>")
```

### Phase 0: Start

1. From the user's input extract the **topic** (one line) and any **context** they gave.
2. `handoff_brainstorm_start(topic, initial_context, project_type?, target_milestones?)`. Pass
   `project_type` and `target_milestones` only when the user gave them (or the project plainly is a
   GUI product, then `gui-app` / `gui-desktop` / `gui-mobile`); omitting them leaves the flow
   unchanged. It returns `brainstorm_id`, `log_doc_id`, `report_doc_id`, `coverage` (plus
   `project_type`, `target_milestones`, `recommended_topics` when applicable). Keep all three ids;
   record them as described in "Suspend and resume".
3. If the topic is too thin to propose anything ("I want to make an app"), ask **one** broad
   question to anchor it, with two or three example directions to react to. Otherwise go straight
   to Phase 1 with a first proposal.

### Phase 1: Dialogue (diverge, then deepen)

**Required topics** — the tracker needs all five `covered`:

| topic_tag | what you are establishing |
|---|---|
| `vision` | what the finished thing is and does |
| `motivation` | why the user wants it; the problem or opportunity |
| `target-users` | who uses it, in what situations |
| `core-value` | what it does better or differently than the alternatives; what must not be lost |
| `scope` | what is in, what is out, what is undecided |

**Optional topics** join the tracker when first discussed. Raise them when the product calls for
them: `tech-constraints`, `business-constraints`, `revenue-model`, `hardware`, `security`,
`regulatory`, or any custom tag that fits. Do not interrogate the user about topics that obviously do
not apply.

**Recommended topics for GUI projects.** When `project_type` matches `gui-*`, the start and status
responses carry `recommended_topics`: `ui-ux-vision`, `wireframe-review`, `visual-identity`,
`gui-verification-strategy`. Promote them from "raise if it fits" to **recommended**: raise each in
Phase 1 once the required topics are in hand, using it as the `topic_tag` (it joins the tracker like
any optional topic). In `gui-verification-strategy`, settle with the user how the GUI will be
verified on the target platform (which environment can run it, how screens are captured, what is
compared against which reference), so that choice is made before requirements are written. When the
responses carry no `recommended_topics` (no `project_type`, or a non-GUI one), do not bring these up.

**Attaching wireframes and mockups (GUI projects).** When the user shows or points to a wireframe,
mockup or screenshot file during Phase 1 (typically under `wireframe-review` or `ui-ux-vision`),
include it in that turn's `handoff_brainstorm_turn` call via `attachments`:
`attachments=[{path: 'wireframes/login.png', kind: 'wireframe', screen: 'login'}]`. `kind` is
`wireframe` | `mockup` | `screenshot`; `screen` is optional and should be the screen name you expect
to use in the finalize `screens` list, so the file is linked to that screen's row in the report. Do
not attach files the user did not provide or mention. Phase 2 research uses them as visual context
(see `brainstorm-execute`).

**Every turn:**

1. Propose or react (principles above). On the first turn of a topic, query memory for the user's
   profile if you have not yet.
2. After the user answers, record the turn **immediately**:

   ```
   handoff_brainstorm_turn(
     brainstorm_id,
     topic_tag="<topic>",
     summary="<what the user said and what you answered, in your words, enough to restore context>",
     decisions=["<what was actually settled this turn>"],     // only real decisions
     open_questions=["<anything still unresolved>"])
   ```

   A turn that carries `decisions` moves its topic to `covered`; a turn without them leaves it
   `in_progress`. Do not invent decisions to push coverage up. If the user agreed to something you
   proposed, that is a decision. If they only reacted ("hmm, maybe"), record it as an open question.
3. Read the return value: `uncovered_topics` tells you where to steer next; `required_covered` is the
   gate signal. Use `coverage_update` only to correct the tracker (for example, a topic the user
   settled implicitly in another turn).
4. Never show the tracker. Move naturally: "Now that we know who it's for, how would they
   actually use it day to day?"

**Gate to Phase 2.** When `required_covered` is `true`:

1. Summarize in plain language, a few lines: what is being built, for whom, why it matters, what is
   in and out.
2. Propose research: "Before we lock this in, I'd like to check three things in parallel: whether it
   is technically realistic, which laws or standards apply to it, and whether something similar
   already exists. I'll also have a skeptical reviewer look for reasons it might not work. This takes
   a few minutes. Shall I go ahead?"
3. Ask with `AskUserQuestion`: *Go ahead and research* / *Keep talking first* / *I'll leave the rest
   to you* (delegate mode).
4. On approval, `handoff_brainstorm_turn(..., phase="research")` (use `topic_tag="scope"` and a
   one-line summary of the approval), then Phase 2. On *Keep talking*, stay in Phase 1.

Do not propose Phase 2 while required topics are uncovered, unless the user asks to skip ahead; then
state which topics are missing and the risk (research built on an incomplete brief), and let them
choose.

### Delegate mode (一任モード)

Triggered when the user says they want you to handle it ("任せる", "あとは AI が調べて", "you decide",
"just research it"), at the start or at the Phase 1 gate.

1. **Continue the brainstorm yourself.** For each uncovered required topic, decide a reasonable
   answer from what the user said and their profile, and record it with `handoff_brainstorm_turn`.
   Label the decision as yours: `decisions=["(AI proposal, user delegated) <decision>"]`. Cover
   optional topics that the product clearly needs.
2. **Respect the limits of delegation.** Business decisions the user would regret (price, legal
   exposure they cannot undo, discarding something they said matters) are not yours to settle
   silently: record them as `open_questions` and carry them to Phase 3 for confirmation.
3. **Fill the coverage** until `required_covered` is `true`, then
   `handoff_brainstorm_turn(..., phase="research")` and launch Phase 2 **without** a further approval
   prompt.
4. In Phase 3 ask for **one final confirmation** of the whole result (scope, priorities, the AI
   proposals, open business questions), not a turn-by-turn review.

### Phase 2: Research workflow

**Executor**: `brainstorm-execute` Workflow, via the skill.

1. Invoke `Skill(handoff-task-loop:brainstorm-execute)` and follow it: it explains how to write the
   `brainstorm_summary` brief from `handoff_brainstorm_status` and project memory, how to launch
   `Workflow(name: 'handoff-task-loop:brainstorm-execute', args: {...})`, how to read the result, and how
   to file findings into report sections 6.1-6.4.
2. While it runs, tell the user it is running and roughly what it covers. Do not invent progress.
3. **Confirm §6 is written before leaving Phase 2.** Check that report sections 6.1-6.4 were filled
   with the research results via `handoff_doc_update_section` (brainstorm-execute SKILL.md §5). The
   engine never writes §6; if it is still `_(未記入)_`, `handoff_brainstorm_finalize` returns a warning
   about it, so fill it now rather than at finalize.
4. When it returns, make sure `handoff_brainstorm_turn(..., phase="convergence")` is recorded (topic
   `scope`, summary: "research complete: <status>").

**Fail closed.** If `status` is `partial` or `failed`, or `review` is `null`, say exactly what is
missing in Phase 3. Never present unreviewed or unverified research as vetted. Never fabricate a
missing result.

### Phase 3: Convergence

**Executor**: you, with the user.

1. **Present the results** in plain language, short:
   - What the research found that changes a decision (technology, existing solutions, competitors).
   - Which regulations or standards apply, what they require, and what needs professional review.
   - The adversarial review: verdict, BLOCKER risks first, then MAJOR; the strongest case for
     stopping or reshaping; what nobody investigated.
   - Anything missing or unverified (`warnings`, failed streams, `review: null`).
2. **Settle with the user**, one decision at a time, each with your recommendation:
   - **Scope**: confirm in-scope and out-of-scope; drop or defer what the research made too costly.
   - **Priorities**: order the in-scope items (what must ship first, what can wait).
   - **Risks**: for each BLOCKER/MAJOR, accept, mitigate (say how), or change the scope.
   - **Undecided**: for each, decide now or deliberately leave it open.
3. Record as you go:
   - Decisions: `handoff_brainstorm_turn(topic_tag="scope"|"priorities"|..., decisions=[...])`.
   - Risks and constraints: `handoff_brainstorm_turn(topic_tag="risks", decisions=[...],
     open_questions=[...])`. The finalized report renders section 7 from decisions recorded under
     tags other than the five required topics.
4. If the user's answers open a large new question the research did not cover, offer another round of
   Phase 2 with a narrowed `tech_investigations` list. Otherwise continue.
5. In delegate mode: present everything once and ask for one confirmation instead of the
   decision-by-decision pass; unresolved business questions are asked here.

Gate: the user agrees scope, priorities and risk handling are settled (or explicitly leaves items
undecided). Then Phase 4.

### Phase 4: Finalize

1. Compose the finalize arguments from the settled state (`handoff_brainstorm_status` for decisions
   and the log for wording):

   ```
   handoff_brainstorm_finalize(
     brainstorm_id,
     executive_summary="<3-6 sentences: what, for whom, why, headline findings and verdict>",
     scope_in=[...],            // at least one; each a concrete, testable-sounding capability
     scope_out=[...],
     undecided=[...],
     priorities=[...],          // highest first
     research_topics=[...],     // questions /research-loop should still answer
     requirement_hints=[...],   // notes for /requirement-loop (constraints, NFR candidates, risks to design around)
     milestone_scope={...},     // only when the session has target_milestones: {"v0-1": ["capability", ...], ...}
     screens=[...],             // only for a gui-* project_type: [{name, function, priority, wireframe_ref}, ...]
     screen_flow=[...],         // only for a gui-* project_type: ["Home -> Settings", ...]
     ux_policy=[...])           // only for a gui-* project_type: ["one-handed use first", ...]
   ```

   When the session was started with `target_milestones` (the status response lists them), also pass
   `milestone_scope`: for each milestone, the `scope_in` capabilities that belong to it. Keys must be
   target milestones; the report's `### 5.5 マイルストーン別スコープ` table lists every target
   milestone, empty ones as `—`. Without `target_milestones`, omit it.

   When `project_type` matches `gui-*`, the report gets a `## 8. GUI 設計概要` section (the priorities
   and hand-off sections move to 9 and 10). Fill it from the dialogue: `screens` lists every screen
   discussed (name, what it does, priority, and the wireframe / mock path when one was reviewed;
   leave a field out when it was not settled), `screen_flow` the transitions between them as text
   lines, `ux_policy` the UX principles agreed. Do not invent screens that were not discussed. For any
   other project type omit all three; they are ignored with a warning.

2. Read the result: `report_doc_id`, `layer`, `item_ids`, `warnings`. If `layer` is `null`, the
   `brainstorm` layer is not declared in `config.toml`; relay the warning. `/requirement-loop` Phase 0
   declares it when it picks the report up, so this is not a blocker.
3. Tell the user, in plain language: the report is saved (give the document name), what it contains
   (summary, scope, research findings, risks, priorities, hand-off notes), and the next step.
   Recommend `/spec-loop` first: it turns the report into diagrams, mockups and a PDF the user can
   review before requirements are written. It is **optional**; the user can go straight to
   `/requirement-loop`, which reads the brainstorm report automatically and will not re-ask what it
   already answers. Present both paths:

   ```
   Recommended:  brainstorm -> spec-loop -> requirement-loop
   Direct:       brainstorm -> requirement-loop   (spec-loop skipped)
   ```

   If `research_topics` is non-empty, mention `/research-loop` for those.
4. Close the session:

   ```
   handoff_save_context(
     session_status="closed",
     summary="Brainstorm complete: <topic summary>",
     decisions=[{ decision: "<scope/priority headline>", confidence: "confirmed|likely", reason: "<evidence>" }],
     handoff_notes=[
       { category: "suggestion",
         note: "Brainstorm <brainstorm_id> finalized. Report doc <report_doc_id>. Next: /spec-loop (optional; recommended), then /requirement-loop. To skip spec-loop, run /requirement-loop directly (report is picked up automatically)." },
       { category: "context", note: "<key findings, open risks, undecided items>" }],
     context_pointers=[{ path: "<report document>", reason: "Brainstorm report" }])
   ```

## Rules

- **Never start research without the user's approval** (or an explicit delegation).
- **Record every dialogue turn** with `handoff_brainstorm_turn`, immediately, with real decisions only.
- **Never expose the coverage tracker** to the user.
- **Always launch research through `Skill(handoff-task-loop:brainstorm-execute)`**; never write an inline
  Workflow script.
- **Never fabricate research results.** Report partial or failed research honestly.
- Regulatory findings are **not legal advice**; keep "needs professional review" items visible.
- Propose; do not interrogate. Technical choices: take a position. Business choices: the user decides.
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only.
- **Do not push.** Stop at the finalized report.
