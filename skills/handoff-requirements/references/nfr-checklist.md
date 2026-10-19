# NFR checklist (ISO/IEC 25010 quality model)

A checklist for finding the non-functional requirements (NFRs) a project
needs, so they are asked for deliberately instead of discovered late. It walks
the eight quality characteristics of the ISO/IEC 25010 (SQuaRE) product
quality model, each with its sub-characteristics and the questions that turn a
vague quality wish into a testable requirement.

- Writing the resulting items: [templates.md](templates.md) (requirement layer) and the
  attribute rules in `SKILL.md`.
- Gate that requires this scan to be finished: [preflight-checklists.md](preflight-checklists.md)
  (gate 2 to 3).

## How to use it

Run the scan once, when the shared contracts of the system are being fixed
(after the top-level requirements and scopes exist, before the feature-level
requirements are decomposed). NFRs are cross-cutting: one NFR usually constrains
many features, so deciding them late means reworking features that are already
written.

For **every** characteristic below:

1. **Does it apply to this project?** Read the questions. If none of them has
   an answer that matters to the stakeholders, record "not applicable" with a
   one-line reason in the NFR document (a short note under a heading without an
   item ID, or in the task notes). Skipping silently is how gaps hide.
2. **If it applies, file an NFR.** One item per concern, in a requirement-layer
   document, with an `NFR-nnn` ID (`NFR` is a default requirement prefix; see
   `SKILL.md` section 2 for the numbering and `handoff_trace_propose` to check for duplicates).
3. **Quantify whenever a number can exist.** "Fast" is not an NFR; "95% of search
   requests answer within 300 ms at 200 concurrent users" is. When the
   stakeholders cannot give a number yet, write the unit and the measurement
   point and mark the value `TBD` explicitly rather than leaving a vague word
   (`trace_lint` flags vague wording through its `ambiguous_word` rule).
4. **Decide the verification method.** Set `- method:` on the item (or on its
   acceptance criteria) to `auto` when a benchmark, scanner, linter or CI check
   can decide pass/fail, and to `manual` when a person has to judge (usability
   review, security review, operational drill). An NFR without a method is
   unverifiable; see the acceptance-criteria rules in `SKILL.md` section 5.
5. **Write acceptance criteria.** Each NFR gets at least one criterion with the
   measurable threshold, the load or condition it holds under, and how it is
   measured (Given/When/Then or EARS; see `SKILL.md` section 5).

```markdown
### NFR-001 Search latency under normal load
- priority: P1
- method: auto

The search endpoint shall answer quickly enough for interactive use.

Acceptance criteria:
- AC-1: Given 200 concurrent users and 100,000 indexed records, when a keyword
  search is issued, then the 95th-percentile response time is at most 300 ms
  (measured by the load-test job in CI).
```

## The eight characteristics

Questions are prompts for the stakeholder conversation, not an exhaustive
list. Add project-specific questions as you go.

### 1. Performance efficiency

| Sub-characteristic | Questions |
|---|---|
| Time behaviour | What response time is acceptable per operation (median and worst case)? What throughput must be sustained? Are there batch windows to fit inside? |
| Resource utilization | What are the memory, CPU, disk, network and battery ceilings? What is the smallest target machine? |
| Capacity | How many concurrent users, records, files or requests per day must be supported now and in a year? What happens beyond the limit (degrade, queue, reject)? |

Typical quantifiers: percentile latency, requests per second, peak memory (MB), data volume.

### 2. Compatibility

| Sub-characteristic | Questions |
|---|---|
| Co-existence | Which other software shares the same host or environment? May this product slow it down or collide with it (ports, files, locks)? |
| Interoperability | Which systems exchange data with this one? Which protocols, formats and versions? Who owns each interface and how are changes announced? |

Typical quantifiers: supported protocol versions, data-format schema versions, backwards-compatibility window.

### 3. Usability

| Sub-characteristic | Questions |
|---|---|
| Appropriateness recognizability | Can a new user tell from the product alone whether it fits their need? |
| Learnability | How long may a first-time user take to finish the main task unaided? Is documentation or onboarding required? |
| Operability | How many steps may a frequent task take? Which input methods (keyboard, touch, voice, CLI) are required? |
| User error protection | What guidance, confirmation or undo is needed for destructive or error-prone actions? How are errors worded? |
| User interface aesthetics | Are there brand, theme or layout constraints? |
| Accessibility | Which standard applies (for example WCAG level)? Which assistive technologies and languages must work? |

Typical quantifiers: task completion rate, time to first success, steps per task, accessibility conformance level.

### 4. Reliability

| Sub-characteristic | Questions |
|---|---|
| Maturity | What defect or failure rate is tolerable in normal operation? |
| Availability | What uptime is required, and in which hours? What planned-maintenance downtime is acceptable? |
| Fault tolerance | Which component failures (disk, network, dependency, bad input) must the product survive, and how (retry, fallback, degrade)? |
| Recoverability | After a crash or data loss, how much data may be lost (recovery point) and how long may recovery take (recovery time)? Are backups and restore drills required? |

Typical quantifiers: availability percentage per month, recovery time and recovery point objectives, mean time between failures.

### 5. Security

| Sub-characteristic | Questions |
|---|---|
| Confidentiality | Which data is sensitive? What must be encrypted at rest and in transit, with which algorithms and key handling? |
| Integrity | How is tampering with data, code or configuration prevented or detected? |
| Non-repudiation | Which actions must be provably attributable to a user (audit log, signature)? How long are the records kept? |
| Accountability | Who can see the audit trail and how is it protected? |
| Authenticity (authentication) | How are users and systems identified (password policy, multi-factor, tokens, certificates)? |
| Authorization | Which roles exist and what may each do? Is least privilege enforced? |
| Compliance | Which regulations or standards apply (privacy law, industry rules)? |

Typical quantifiers: encryption strength, session lifetime, audit retention period, number of unresolved high-severity scanner findings (target zero).

### 6. Maintainability

| Sub-characteristic | Questions |
|---|---|
| Modularity | Which components must be independently changeable or replaceable? |
| Reusability | Which parts are meant to be reused by other products? |
| Analysability | What logging, tracing and diagnostics are needed to find a fault in production? |
| Modifiability | How quickly must a typical change or hot fix be deliverable? Are there coding or dependency policies? |
| Testability | What test coverage and automation level is required? Can each component be tested in isolation? What build and test time is acceptable? |

Typical quantifiers: coverage threshold, maximum build time, maximum cyclomatic complexity, dependency-update cadence.

### 7. Portability

| Sub-characteristic | Questions |
|---|---|
| Adaptability | Which operating systems, browsers, devices, locales and runtime versions must be supported? |
| Installability | How is it installed, upgraded and uninstalled (package, container, installer, store)? Can it run offline? What is the acceptable install time? |
| Replaceability | Can it replace an existing product? What data must be migrated, and can it be rolled back? |

Typical quantifiers: supported-platform matrix with versions, upgrade path from version N-1, migration downtime.

### 8. Functional suitability

| Sub-characteristic | Questions |
|---|---|
| Functional completeness | Which tasks and use cases must be fully covered? What is explicitly out of scope? |
| Functional correctness | What accuracy or precision is required (rounding, units, tolerance)? Which data-integrity rules must always hold? |
| Functional appropriateness | Does each function help the user reach the goal directly, without needless steps? |

Typical quantifiers: numeric tolerance, number of invariants that must hold, share of use cases covered by acceptance criteria.

## Completion check

Before leaving the scan:

- All eight characteristics were walked, each either with at least one NFR or a recorded "not applicable" reason.
- Every NFR has a measurable threshold (or an explicit `TBD` with owner and date), a `method`, and at least one acceptance criterion.
- `trace_lint` reports no `ambiguous_word` or `missing_acceptance` finding for the NFR items.
- Conflicting NFRs (for example strict latency against strong encryption on a small device) are called out and the priority order is decided.
