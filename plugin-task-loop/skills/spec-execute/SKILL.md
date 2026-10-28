---
name: spec-execute
description: Specification phase reference for /spec-loop (triggers - spec-loop, 要求仕様, specification, 仕様策定, spec phase). How the coordinator turns an approved scope into Mermaid architecture diagrams, HTML GUI mockups with a stateDiagram-v2, use cases with sequenceDiagram, and a PDF requirements specification.
---

# spec-execute — Phases 1-4 generation reference

Reference for the **spec-loop coordinator** (`/spec-loop`). Phase 0 (import, scope approval) and the
user gates are described in `commands/spec-loop.md`; this skill covers what happens between the
gates: how to brief the generation agents, which Mermaid type each phase uses, how to validate and
revise, and how to assemble, save and export the specification.

Trigger words for this phase: `spec-loop`, `要求仕様`, `specification`, `仕様策定`, `spec phase`.
When the user says one of them after a brainstorm, start `/spec-loop` Phase 0.

## 1. Agents and ground rules

| Phase | Agent (Sonnet) | Produces |
|---|---|---|
| 1 | `spec-architect` | Mermaid `flowchart` / `C4Context`, components, interfaces, data flows |
| 2 | `spec-gui-designer` | one HTML mock per screen, Mermaid `stateDiagram-v2` of transitions |
| 3 | `spec-usecase-analyst` | use cases per scope item, Mermaid `sequenceDiagram` for key flows |

- Agents are read-only with respect to handoff state. They get everything through the brief (their
  `Input` table) and return a structured report. **You** persist: the specification document, the
  session notes, every handoff call.
- Launch an agent with the Agent tool and its name; put the whole brief in the prompt. They see no
  dialogue history. A brief that omits the approved scope or the user's corrections yields generic
  output.
- The brief carries the **approved** inputs only: after Phase 0 that is the scope list (`BS-*` id or
  `(added)`, title, priority), hints and constraints; after Phase 1 also the architecture summary;
  after Phase 2 also the screens and transitions.
- A report with `status: blocked` or `needs_input` is not output: resolve the question (usually
  with the user) and re-run. Never fill the gap yourself and present it as the agent's result.
- Regeneration after a user correction passes the user's words verbatim in `feedback` and the
  previous output, and regenerates the whole artifact of that phase (a diagram is cheap to redraw
  and a patched one drifts).

## 2. Phase 1 — architecture

Templates: `references/diagram-templates.md` (copy the shape, replace the content).

### Steps

1. **Collect the input** from the approved Phase 0 result: every in-scope item (`BS-SCOPE-NNN` id or
   `(added)`, title, priority), the `BS-HINT-*` hints, and `constraints` (technology, platform,
   budget, settled technical decisions of the brainstorm). Out-of-scope items are not passed.
2. **Brief `spec-architect`** with `scope_items`, `constraints`, `feedback` (empty on the first run),
   `response_language`. A `blocked` or `needs_input` report goes back to the user, not to the next
   step.
3. **Check the diagram** against the content rules below. Re-run `mmdc` yourself on any diagram the
   agent marked `unverified` when `mmdc` exists (section 6).
4. **Save the intermediate result** (the document is a working draft; Phase 4 saves the final
   specification as a separate document):
   `handoff_doc_save(slug="spec-<topic-slug>-architecture", title="Architecture: <topic>", doc_type="design", tags=["spec-loop", "spec-intermediate", "phase-1"], body=<diagrams + components + interfaces + flows>)`.
   Keep the `doc_id` in the session notes; a regeneration passes that `doc_id` and the full new body.
5. **Present** (gate below), then carry the approved diagram and component list forward into the
   Phase 2 and Phase 3 briefs.

### Diagram type (REQ-SL-010)

- `flowchart TD` (default): component structure and data flow. One `subgraph` per subsystem or
  trust boundary, labelled edges for data, a distinct node shape for external actors and systems
  (stadium `([...])`) and for data stores (cylinder `[(...)]`).
- `C4Context`: when people and external systems dominate the picture. Add a `flowchart TD` for the
  internals of the system in the middle.
- Optional `erDiagram` / `classDiagram` only when the scope calls for a data model or domain
  structure.

### Content rules

The result must show, and the agent's report must list in tables next to the diagram:

- **subsystem boundaries**: a `subgraph` (or `System_Boundary`) per boundary, named by what it is
  (device, service, app, trust zone);
- **main components**, each tracing to at least one `BS-SCOPE-*` item (`(added)` items trace to
  "added"); scope items the agent could not place are listed, never dropped;
- **data flows**: every edge is labelled with what flows, and the arrow shows the direction;
- **external interface** list: counterparty, protocol or format, direction.

A diagram with no external interface, or with a component without a data flow in or out, is
incomplete: send it back to the agent with that as `feedback` before the user sees it. Keep it to
about 15 nodes; split by subsystem otherwise.

### Presenting and the approval gate

Show the user, in this order: the diagram (as the Mermaid code block, plus the SVG path if you
rendered one), three or four plain sentences ("the device sends readings to a gateway, the gateway
stores them, the phone app reads them"), the unplaced scope items, and the agent's open questions.

Gate with `AskUserQuestion`: *Approve* / *Change it* / *Suspend*.

- *Change it*: collect the change request as free text. Pass the user's words verbatim as `feedback`
  together with the previous diagram and **regenerate the whole diagram** (Phase 1 is cheap to
  redraw and a patched diagram drifts). Save the new version to the same `doc_id`, present again.
- Repeat until *Approve*. If the user keeps changing the scope itself (an item added or dropped),
  return to the Phase 0 scope gate instead of absorbing it here.
- *Suspend*: record the resume note exactly as `commands/spec-loop.md` says; the intermediate
  document stays as the draft.

## 3. Phase 2 — GUI mockups and transitions

Templates and rules: `references/mock-guidelines.md` (HTML), `references/diagram-templates.md`
(`stateDiagram-v2`).

### Skip decision

Decide before briefing anything. The phase applies when Phase 0 found a GUI: the brainstorm report
had a `GUI 設計概要` section with a non-empty `screens` table, or the user listed screens in manual
mode. The phase is skipped when **both** are true: `screens` is empty or absent, and the user does
not ask for a GUI (CLI tools, libraries, backend services, batch jobs). Then:

1. Tell the user "no screens were defined, so Phase 2 is skipped", and ask once whether they want
   one anyway (`AskUserQuestion`: *Skip* / *Define screens now*); *Define screens now* is the manual
   elicitation of Phase 0 step 5 for screens only.
2. On skip: do not launch `spec-gui-designer`, do not save a Phase 2 document, and record
   "Phase 2 skipped: no GUI" in section 4 of the specification and in the session notes. Phase 3
   then receives an empty `screens`.

If `spec-gui-designer` itself returns `status: skipped`, treat it the same way.

### Steps

1. **Brief `spec-gui-designer`** with `screens` (id, name, purpose, main elements, linked `BS-*`),
   `screen_flow`, `ux_policy`, the approved Phase 1 architecture summary, `output_dir`, `feedback`,
   `response_language`. Choose `output_dir` inside the project's export area so the files are
   findable, for example `.handoff/exports/spec-<topic-slug>/mocks/` (derived files; the
   specification document references them by path).
2. **Check the output** with the review checklist in `references/mock-guidelines.md`: a file per
   screen, self-contained (inline CSS, no network, no scripts that fetch), relative links between
   mocks that all resolve.
3. **Transition diagram** (REQ-SL-010): Mermaid `stateDiagram-v2`, screens as states, user actions
   as edge labels, start/end states, and the error/empty states the mocks show. Every `screen_flow`
   line from the brainstorm must appear; transitions and screens the agent added are flagged to the
   user as proposals, not presented as settled. Validate with `mmdc` (section 6).
4. **PNG for the PDF**: rasterize each mock to PNG when a headless browser is available (the
   Chrome the tool check found) so the specification can embed `![screen](path.png)`; otherwise
   list the HTML paths and say the PDF has no images of them.
5. **Save the intermediate result**:
   `handoff_doc_save(slug="spec-<topic-slug>-screens", title="Screens: <topic>", doc_type="design", tags=["spec-loop", "spec-intermediate", "phase-2"], body=<screens table + mock file paths + stateDiagram-v2 + cross-check>)`.
   Keep the `doc_id`; revisions pass it with the full new body.

### Presenting and the approval gate

Present the mock file paths (absolute, so the user can open them in a browser), the screens table
with the linked `BS-*`, the transition diagram, the cross-check result and any added screens.

Gate with `AskUserQuestion`: *Approve* / *Change it* / *Suspend*.

- *Change it*: collect revisions per screen (or on the flow). Regenerate **only the screens that
  changed plus the transition diagram** by briefing the agent with the user's words as `feedback`
  and listing the unchanged screens as "keep as is". Re-run the cross-check, update the same
  `doc_id`, present again.
- A change that adds or removes a screen changes the scope of Phase 2 only; confirm it with the
  user and note it. A change that adds or drops a scope item goes back to the Phase 0 gate.
- *Suspend*: record the resume note as `commands/spec-loop.md` says.

## 4. Phase 3 — use cases

Goal: every in-scope item is described as a use case the user can read and correct, with a sequence
diagram where several parties interact (REQ-SL-005).

### Brief

Brief for `spec-usecase-analyst`: `scope_items` (approved `BS-*` ids, titles, priorities, hints),
`architecture` (the approved Phase 1 components and interfaces, so lifelines use real component
names), `screens` (the approved Phase 2 screens and transitions; empty without a GUI), `feedback`,
`response_language`. Pass approved output only.

### Use case template

The agent returns one block per use case; keep this shape in the specification (section 5), filled
in the user's language:

```
### UC-<n>: <title>
Realizes: BS-SCOPE-NNN[, BS-SCOPE-MMM]          (or "(added in spec-loop)")
Components: <Phase 1 component names>    Screens: <Phase 2 screen names, or "none">
- actor: <primary actor>; supporting: <other actors, external systems>
- preconditions: <what must hold before the use case starts>
- main scenario:
  1. <actor action / system response, each step tied to a component or screen>
- alternative scenarios:
  - <valid variation, "at step N: ...">
- exceptions:
  - <failure at step N and how the system responds>
- postconditions: <what holds afterwards>
```

Rules for the template:

- One use case per distinct user goal. An item that hides several goals gets several `UC-<n>`; a
  pure constraint or non-functional goal is not forced into a use case (it goes to section 6 as an
  NFR candidate).
- `UC-<n>` ids are stable across re-runs and revisions; new use cases take the next free number and
  a removed one leaves a gap.
- Steps are numbered, written as observable behaviour ("the app shows the list of devices"), not as
  implementation.
- Alternative and exception entries name the step they branch from, so the sequence diagram's
  `alt` / `opt` blocks can mirror them.
- Every field is present; write "none" instead of dropping it, so a reader knows it was considered.

### Sequence diagrams (REQ-SL-010)

Draw a Mermaid `sequenceDiagram` for a use case when it involves two or more participants, or has a
non-trivial exception path; a one-actor, one-step use case needs none.

- Participants are the Phase 1 component names, spelled as in the architecture diagram. Declare
  them first: `actor User as User` for people and external parties, `participant Api as API server`
  for components. Add the id/label split so labels can contain spaces.
- Use `->>` for calls and `-->>` for returns. Start with `autonumber` so the diagram steps line up
  with the numbered main scenario.
- Show the main flow top to bottom; render alternatives and exceptions with `alt` / `else` / `end`
  and `opt` blocks, labelled with the condition from the use case text.
- Keep each diagram to about 8 participants and 20 messages. Larger flows are split by sub-goal
  into separate use cases rather than one unreadable diagram.
- Message text avoids `;`, `#` and unbalanced quotes (they break the parser) and does not use the
  bare word `end` in lowercase inside a label.
- Validate every diagram with `mmdc` (section 6) before the user sees it.

### Coverage check (yours, before the gate)

Do not forward the agent's report unchecked. Compare it with the approved inputs:

- **missing**: an in-scope `BS-*` id without a use case (or an explanation in the agent's "Items not
  expressible as use cases"), or a Phase 2 screen that no use case touches (every screen must be
  used by some use case);
- **duplicate**: two use cases with the same actor and goal, or the same main scenario under two
  titles;
- **unplaced**: a use case whose `Realizes:` line names no scope item; an actor, component or
  screen that is not in the approved inputs (the agent flags these as proposals); steps that
  contradict the architecture.

### Gate

1. Show the user the gaps and duplicates you found first, then the use case list (id, title,
   actor, realized scope item) with the sequence diagrams.
2. `AskUserQuestion`: *Approve* / *Fix gaps or duplicates* (take the user's words, pass them
   verbatim as `feedback`, regenerate the affected use cases, repeat the coverage check) /
   *Suspend*. The user may also add a missing use case themselves; mark it `(added)`.
3. On Approve **create** the specification document, so a suspend loses nothing. No spec document
   exists before this point: the Phase 1 and Phase 2 documents are separate intermediate design
   documents (`spec-<topic-slug>-architecture`, `spec-<topic-slug>-screens`, tags
   `spec-intermediate`) and must never be overwritten or retagged. Call
   `handoff_doc_save(slug="spec-<topic-slug>", title, doc_type="spec", tags=["spec-loop", "requirements-spec"], body=<sections 1-5 filled, section 6 still empty>)`
   with no `doc_id`, keep the returned `doc_id` as the spec `doc_id`, then record
   `spec-loop: spec_doc=<doc_id> brainstorm_report=<report_doc_id|none> phase_done=3` in the
   session notes. Phase 4 updates this same `doc_id`. (Before this point the notes carry
   `spec_doc=none` and the intermediate document ids.)

## 5. Phase 4 — assemble, save, export

Goal: one approved specification document plus a PDF, handed to `/requirement-loop` (REQ-SL-006).

### Preflight

Before assembling, check the inputs are complete:

- Phases 0, 1, 3 have an approved result; Phase 2 is approved or recorded as "Phase 2 skipped: no
  GUI". Never assemble from unapproved output.
- Every in-scope `BS-*` id appears in the scope table and in at least one use case (or in the
  explained-gaps list that goes to section 6).
- Every Mermaid diagram was validated, or section 6 will say "diagrams not validated".
- The Phase 2 mock PNGs exist, or the section says which screens are shown as paths only.

### Document layout

One Markdown document, language of the user, headings in this order:

```
# 要求仕様書: <topic>            (or "Requirements Specification: <topic>")
## 1. 概要                       purpose, actors, source brainstorm report (doc id), status
## 2. スコープ                   in-scope table (BS-* id | title | priority), out-of-scope, undecided
## 3. システムアーキテクチャ      ```mermaid blocks + components, interfaces, data flows
## 4. 画面設計                   screens table, mock images/paths, ```mermaid stateDiagram-v2  (or "Phase 2 skipped: no GUI")
## 5. ユースケース               one subsection per use case (section 4 template), ```mermaid sequenceDiagram blocks
## 6. 制約・未決事項             constraints, NFR candidates (from BS-HINT), open questions, skipped phases
```

- Keep the `BS-*` ids in the scope table and in each use case ("Realizes: BS-SCOPE-002") so
  `/requirement-loop` can link requirements back with `refines`.
- Do not define a heading of the form `#### BS-...` in this document: those ids belong to the
  brainstorm report and would collide.
- Section 3 may carry an `erDiagram` (entities and relations) or a `classDiagram` (domain
  structure) when the scope calls for a data model, next to the component diagram; omit both for
  projects without a persistent data model.
- Mermaid goes in fenced ```` ```mermaid ```` blocks; the PDF pipeline renders them to SVG.
  Mock images are `![screen name](path.png)` with paths relative to the project directory.
- Section 1 states the status (`draft` until the final gate, `approved` after it).

### Save and export

1. Update the specification document created at the Phase 3 gate:
   `handoff_doc_save(doc_id=<spec doc_id>, body=<full assembled body>, tags=["spec-loop", "requirements-spec"])`
   (`slug`, `title` and `doc_type` stay as created). If it does not exist yet (Phase 3 gate
   skipped, or a resume with `spec_doc=none`), create it instead:
   `handoff_doc_save(slug="spec-<topic-slug>", title, doc_type="spec", tags=["spec-loop", "requirements-spec"], body=...)`
   and keep the returned `doc_id` in the session notes. Never reuse the `doc_id` of the Phase 1 or
   Phase 2 intermediate documents. Revisions pass the spec `doc_id` (and the full new `body`).
   `tags` replace the old list on update, so always send the complete list (the approval step adds
   `approved` to it).
2. `handoff_doc_export(doc_id=<doc_id>, format="pdf", template="spec")` writes
   `.handoff/exports/<slug>.pdf` (`pdf_path` in the result). Mermaid blocks are rendered when `mmdc`
   is installed; otherwise they stay code blocks and the call returns warnings. No Typst (not built in, no CLI): the
   call fails with install instructions; relay them and let the user choose to install and retry,
   or to review the document without a PDF.
3. Read `documents[].warnings` and `warnings` and report them. Do not claim the PDF contains
   diagrams or mockup images when a warning says otherwise. Re-export after every revision so the
   PDF never lags behind the document.
4. Record `spec-loop: spec_doc=<doc_id> brainstorm_report=<report_doc_id|none> phase_done=4` in
   the session notes.

### Final gate

Present: document name and `doc_id`, the PDF path (or "no PDF: <reason>"), a one-line summary of
each section, warnings, open questions. Then `AskUserQuestion` with four options:

| Option | Meaning | What you do |
|---|---|---|
| **Approve** | the specification is accepted | continue with the hand-off below |
| **Revise** | something must change | ask what; route to the phase that owns it: scope -> Phase 0, architecture or data model -> Phase 1, screens or transitions -> Phase 2, use cases -> Phase 3, wording or layout only -> edit in Phase 4. Regenerate that phase with the user's words as `feedback`, re-run later phases whose input changed (a scope or architecture change reaches the use cases), re-assemble, re-save, re-export, return to this gate |
| **Suspend** | pause, resume later | save the note `spec-loop: spec_doc=<doc_id> ... phase_done=4 awaiting=final_gate` with `handoff_save_context`, leave the document without `approved`, stop |
| **Stop** | abandon | end without approval; say the document stays saved but is not approved; no `approved` tag, no hand-off |

Only Approve starts the hand-off. Never treat silence or a vague answer as approval; ask again.

### Hand-off to requirement-loop (on Approve)

1. Mark the document: `handoff_doc_save(doc_id=<doc_id>, body=<same body with status approved in
   section 1>, tags=["spec-loop", "requirements-spec", "approved"])` (tags replace, so send all).
2. Close the session with the hand-off notes, naming the document and PDF so the next session needs
   no search:

   ```
   handoff_save_context(session_status="closed", summary="Spec complete: <topic>",
     handoff_notes=[{ category: "suggestion",
       note: "spec-loop: spec_doc=<doc_id> brainstorm_report=<report_doc_id|none> status=approved pdf=<path>. Next: /requirement-loop; Phase 0 reads this spec next to the brainstorm report." }],
     context_pointers=[{ path: "<spec document or slug>", reason: "Approved requirements specification" }])
   ```

3. What `/requirement-loop` Phase 0 takes from the approved specification (find it by the
   `spec_doc=` note, or `handoff_doc_list` for tag `requirements-spec` + `approved`):

   | Specification section | Used for |
   |---|---|
   | 1 概要 | purpose and actors: confirm, do not re-ask |
   | 2 スコープ | requirement candidates; each derived requirement keeps `- refines: BS-SCOPE-NNN` |
   | 3 アーキテクチャ | Phase 1 architecture input, interfaces and data model |
   | 4 画面設計 | the GUI project flow (screens, transitions, mock paths) |
   | 5 ユースケース | one or more requirements per `UC-<n>`; its exceptions seed acceptance criteria |
   | 6 制約・未決事項 | NFR candidates and the open-question list for the Phase 1 interview |

4. Tell the user the next step is `/requirement-loop`. Do not start it yourself and do not push.

## 6. Mermaid validation

Every diagram is checked before the user sees it:

```bash
mmdc -i diagram.mmd -o diagram.svg      # binary from HANDOFF_MMDC_BIN when set
```

- Write the temp files outside the project tree. A non-zero exit means a syntax error: fix and
  re-run before presenting.
- Supported types: `flowchart`, `sequenceDiagram`, `stateDiagram-v2`, `C4Context`, `erDiagram`,
  `classDiagram`. Phase defaults: Phase 1 `flowchart`/`C4Context`, Phase 2 `stateDiagram-v2`,
  Phase 3 `sequenceDiagram`. Add `erDiagram` (data model) or `classDiagram` (domain structure) only
  where the scope calls for them, normally as part of Phase 1.
- `mmdc` missing: diagrams are `unverified`. Say so to the user and write "diagrams not validated"
  in section 6 of the specification.

Third-party tools (`mmdc`, headless Chrome, Typst) are checked once at the start of Phase 0 using
`agents/_spec-loop-tool-check.md`; re-run the check when the user says they installed something.

## Rules

- Briefs contain approved inputs only; never pass unapproved output to the next agent.
- Never fabricate scope, screens, `BS-*` ids, render results or a PDF.
- Carry agent warnings (unverified diagrams, unplaced scope items, extra transitions) to the user.
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only; the export directory is
  written by `handoff_doc_export` and the agents' declared `output_dir`.
- Do not commit or push.
