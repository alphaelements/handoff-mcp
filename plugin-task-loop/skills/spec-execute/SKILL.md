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

Brief for `spec-architect`: `scope_items`, `constraints` (technology, platform, budget; from the
hints and the brainstorm's settled technical decisions), `feedback`, `response_language`.

Diagram type (REQ-SL-010):

- `flowchart` (default): component structure and data flow. Use `subgraph` for subsystem boundaries,
  labelled edges for data, a distinct node shape for external interfaces.
- `C4Context`: when people and external systems dominate the picture. The agent may add a
  `flowchart` for the internals.

The result must show subsystem boundaries, main components, data flows and external interfaces, and
trace each component to `BS-*` items. Present to the user: the diagram, then three or four plain
sentences ("the device sends readings to a gateway, the gateway stores them, the phone app reads
them"), then any unplaced scope items. Gate: approve, or take change requests and regenerate.

## 3. Phase 2 — GUI mockups and transitions

Skip when the project has no GUI (no `screens` from the report and none from the user): CLI tools,
libraries, backends. The skip is recorded in the specification as "Phase 2 skipped: no GUI".

Brief for `spec-gui-designer`: `screens` (id, name, purpose, main elements, linked `BS-*`),
`screen_flow`, `ux_policy`, the Phase 1 architecture summary, `output_dir`, `feedback`,
`response_language`. Choose `output_dir` inside the project's export area so the files are findable,
for example `.handoff/exports/spec-<topic-slug>/mocks/` (derived files; the specification document
references them by path).

- One self-contained HTML file per screen (inline CSS, no network, no scripts that fetch).
- Transitions as Mermaid `stateDiagram-v2` (screens are states, user actions label the edges).
  Every `screen_flow` line from the brainstorm must appear; extra transitions the agent adds are
  flagged as proposals.
- For the PDF, rasterize each mock to PNG when a headless browser is available (the Chrome the
  tool check found), so the specification can embed `![screen](path.png)`; otherwise list the HTML
  paths and say the PDF has no images of them.

Present the mock file paths and the transition diagram; collect revisions per screen; regenerate
only the screens that changed plus the diagram.

## 4. Phase 3 — use cases

Brief for `spec-usecase-analyst`: `scope_items`, the architecture (so lifelines use real component
names), `screens` (empty without a GUI), `feedback`, `response_language`.

Per scope item: actor, preconditions, main scenario, alternative scenarios, exceptions. For flows
that cross several components, a Mermaid `sequenceDiagram` (participants named as in the
architecture diagram). Before the gate, **check coverage yourself**: every in-scope item has at
least one use case; no two use cases describe the same goal; every screen is used by some use case.
Show the user the gaps and duplicates you found, then the list. Gate: approve or fix.

## 5. Phase 4 — assemble, save, export

### Document layout

One Markdown document, language of the user, headings in this order:

```
# 要求仕様書: <topic>            (or "Requirements Specification: <topic>")
## 1. 概要                       purpose, actors, source brainstorm report (doc id), status
## 2. スコープ                   in-scope table (BS-* id | title | priority), out-of-scope, undecided
## 3. システムアーキテクチャ      ```mermaid blocks + components, interfaces, data flows
## 4. 画面設計                   screens table, mock images/paths, ```mermaid stateDiagram-v2  (or "Phase 2 skipped: no GUI")
## 5. ユースケース               one subsection per use case, ```mermaid sequenceDiagram blocks
## 6. 制約・未決事項             constraints, NFR candidates (from BS-HINT), open questions, skipped phases
```

- Keep the `BS-*` ids in the scope table and in each use case ("Realizes: BS-SCOPE-002") so
  `/requirement-loop` can link requirements back with `refines`.
- Do not define a heading of the form `#### BS-...` in this document: those ids belong to the
  brainstorm report and would collide.
- Mermaid goes in fenced ```` ```mermaid ```` blocks; the PDF pipeline renders them to SVG.

### Save and export

1. `handoff_doc_save(slug="spec-<topic-slug>", title, doc_type="spec", tags=["spec-loop", "requirements-spec"], body=...)`.
   Keep the returned `doc_id` in the session notes. Revisions pass that `doc_id` (and the full new
   `body`) to update the same document. `tags` replace the old list on update, so always send the
   complete list (the approval step adds `approved` to it).
2. `handoff_doc_export(doc_id, template="spec")` writes `.handoff/exports/<slug>.pdf`. Mermaid
   blocks are rendered when `mmdc` is installed; otherwise they stay code blocks and the call
   returns warnings. Typst missing: the call fails with install instructions; relay them.
3. Read `documents[].warnings` and `warnings` and report them. Do not claim the PDF contains
   diagrams or mockup images when a warning says otherwise.

### Final gate and hand-off

*Approve* / *Revise* / *Suspend* / *Stop*, exactly as in `commands/spec-loop.md` Phase 4. On
Approve the connection to `/requirement-loop` is: the approved specification document is tagged
`approved` and named in the closing session note, so `/requirement-loop` Phase 0 can read its
architecture, screens and use cases next to the brainstorm report.

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
