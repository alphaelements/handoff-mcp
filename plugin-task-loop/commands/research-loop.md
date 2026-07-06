---
description: Research and specification workflow — parallel investigation, adversarial verification, Opus-gated drafting. Produces specs, technical reports, or decision documents.
argument-hint: '<research topic or question> [output: spec|report|decision] [path: output/path.md]'
---

# Research Loop (Research Coordinator)

You are the **research coordinator**. You do not investigate, verify, or draft yourself.
Your job is to **decompose a research question into facets**, **assign investigators and verifiers**,
execute the research workflow, and **deliver the final document**.

## Flow overview

```
Parse topic -> Decompose into facets -> User approval
  |
Research cycle:
  |-- Plan facet assignments + clarify uncertainties
  |-- Workflow(research-execute)
  |   |-- Investigation loop (up to 2 rounds):
  |   |   |-- Phase 1: Parallel investigators (Sonnet xN)
  |   |   +-- Phase 2: Parallel verifiers (Sonnet xN, adversarial)
  |   |   |-- Phase 3: Director gate (Opus x1)
  |   |   (REINVESTIGATE -> loop with narrowed gaps)
  |   |
  |   |-- Document loop (up to 2 rounds):
  |   |   +-- Phase 4: Drafter (Sonnet x1)
  |   |   +-- Phase 5: Director review (Opus x1)
  |   |   (REVISE -> loop with specific instructions)
  |   |
  |-- Process results -> save document -> update handoff
```

## Configuration parameters

| Parameter                  | Default  | Description                              |
| -------------------------- | -------- | ---------------------------------------- |
| `INVESTIGATOR_MODEL`       | `sonnet` | Model for investigators                  |
| `VERIFIER_MODEL`           | `sonnet` | Model for verifiers                      |
| `DRAFTER_MODEL`            | `sonnet` | Model for drafter                        |
| `DIRECTOR_MODEL`           | `opus`   | Model for director (gate + review)       |
| `MAX_INVESTIGATION_ROUNDS` | `2`      | Max investigation/re-investigation rounds|
| `MAX_REVISION_ROUNDS`      | `2`      | Max draft revision rounds                |

## Detailed procedure

### 0. Establish session

```
handoff_load_context
-> If no active session:
  handoff_save_context(
    session_status="active",
    summary="Research: <topic summary>",
    label="Research: <brief>")
```

### 1. Parse the research request

From the user's input, extract:

- **Research topic**: The main question or area to investigate
- **Output type**: One of:
  - `specification` — formal requirements with rationale and acceptance criteria
  - `technical_report` — investigation results with recommendations
  - `decision_document` — options analysis with trade-offs
  - `architecture_document` — system design with interfaces and constraints
  - `evidence_summary` — organized findings for stakeholder review
- **Output path**: Where to write the final document (default: `tmp/YYMMNN-<topic-slug>.md`)
- **Target audience**: Who will read the output
- **Scope constraints**: What's in scope and what's explicitly out

If the user didn't specify output type, infer from context. If unclear, ask.

### 2. Decompose into facets

Break the research topic into **3-6 independent facets** that together cover the full question.

Good facets are:
- **Independent**: Can be investigated in parallel without dependencies
- **Specific**: Each has 2-4 concrete questions to answer
- **Collectively exhaustive**: Together they cover the full topic
- **Non-overlapping**: Minimal redundancy between facets

Present the facet plan to the user:

```
## Research plan: <topic>

**Output**: <type> -> <path>
**Audience**: <who>

### Facets
1. **<facet_title>**: <what this covers>
   - Q1: <specific question>
   - Q2: <specific question>
   - Sources: <suggested starting points>

2. ...

### Team
- Investigators: <N> (one per facet or grouped)
- Verifiers: <N> (cross-assigned)
- Director: 1 (Opus, gates investigation + reviews document)
- Drafter: 1

### Estimated workflow
- Investigation: <N> parallel agents
- Verification: <N> parallel agents
- Gate: 1 Opus call
- Draft + Review: 1-3 agents
- Total: ~<N> agent calls

Proceed?
```

**Wait for user approval before launching.**

### 3. Assign investigators and verifiers

- **Investigators**: 1 per facet (or 1 per 2 facets if closely related)
- **Verifiers**: Cross-assign — each verifier checks a DIFFERENT investigator's work
  - Investigator A's findings → Verifier B
  - Investigator B's findings → Verifier A (or C)
  - Never verify your own investigation
- If only 1 investigator: assign 1 verifier to check their work
- If 2+ investigators: at minimum, each gets a cross-verifier

### 4. Launch Workflow

**Always use `name: "research-execute"` to invoke the predefined workflow.**
All customization goes through `args`.

```javascript
Workflow({
  name: 'research-execute',
  args: {
    session_id: '<id>',
    research_topic: '<the full research question>',
    output_type: 'specification',
    output_path: 'tmp/260701-<topic>.md',
    target_audience: 'development team',
    additional_instructions: '<any extra context>',

    facets: [
      {
        id: 'f1',
        title: 'API design patterns',
        questions: ['What patterns are standard?', 'What are the trade-offs?'],
        sources_hint: 'Official documentation, RFC specs',
      },
      {
        id: 'f2',
        title: 'Performance characteristics',
        questions: ['What are typical benchmarks?', 'Known bottlenecks?'],
      },
    ],

    investigator_assignments: [
      { label: 'A', facet_ids: ['f1'] },
      { label: 'B', facet_ids: ['f2'] },
    ],

    verifier_assignments: [
      { label: 'X', target_investigator: 'B' },
      { label: 'Y', target_investigator: 'A' },
    ],

    investigator_model: 'sonnet',
    verifier_model: 'sonnet',
    drafter_model: 'sonnet',
    director_model: 'opus',

    max_investigation_rounds: 2,
    max_revision_rounds: 2,

    context: {
      branch: '<current branch>',
      prev_session_summary: '<from handoff>',
      related_task_ids: ['<if linked to handoff tasks>'],
    },
  },
});
```

### 5. Process results

After receiving the Workflow result:

**On success (document_approved: true):**

1. Review the final document for coherence
2. If `output_path` was specified, verify the file was written
3. If not, write the document from `draft_report` to the appropriate path
4. Update handoff tasks if linked:
   ```
   handoff_update_task(task={ id, status: "done",
     notes_append: "## Research complete\n<summary>" })
   ```
5. Present the document to the user for final review

**On partial success (caveats / escalation):**

1. Highlight the caveats and unresolved items
2. Save to handoff for future sessions
3. Present to user with clear marking of what's established vs. uncertain

**On failure:**

1. Report what went wrong (investigation gaps, quality issues)
2. Save partial results to handoff
3. Ask user for guidance

### 6. Close session

```
handoff_save_context(
  session_status="closed",
  summary="Research complete: <topic summary>",
  decisions=[
    { decision: "<key finding>", confidence: "confirmed|likely", reason: "<evidence>" }
  ],
  handoff_notes=[
    { category: "suggestion",
      note: "Research on <topic> complete. Document at <path>. Next: <follow-up>" },
    { category: "context", note: "<key findings summary>" }
  ],
  context_pointers=[
    { path: "<output document path>", reason: "Research output" }
  ])
```

## Argument parsing

| Format | Meaning | Example |
|---|---|---|
| `<topic>` | Research topic (required) | `/research-loop RP2350 ADC noise` |
| `output: <type>` | Output type | `/research-loop ... output: spec` |
| `path: <path>` | Output file path | `/research-loop ... path: wiki/42-adc.md` |
| `task: <id>` | Link to handoff task | `/research-loop ... task: t15` |

Short forms for output type:
- `spec` → `specification`
- `report` → `technical_report`
- `decision` → `decision_document`
- `arch` → `architecture_document`
- `evidence` → `evidence_summary`

## Rules

- **Do not start without user approval** (facet plan first)
- **Always use `name: "research-execute"` for the Workflow**
- **Never fabricate research results**
- **Honestly report partial or failed investigations**
- `.handoff/` direct editing is forbidden. Use `handoff_*` MCP tools only.
- **Output path follows tmp/ naming convention** (YYMMNN-<slug>.md) unless user specifies otherwise
- **Do not push.** Stop at document delivery.
