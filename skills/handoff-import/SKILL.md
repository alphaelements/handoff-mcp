---
name: handoff-import
description: "Import existing handoff documents into .handoff/ management. Reads the specified file, structures its content into tasks/decisions/blockers/notes, and calls handoff_import_context in one shot. Triggers on '/handoff-import <path>', 'import handoff', 'take this into handoff'."
---

# Handoff Import Skill

Import an existing handoff document (Markdown, JSON, or free-text) into
structured `.handoff/` management via a single `handoff_import_context` call.

## Procedure

1. Read the source document with the Read tool.
2. Analyze the content and decompose it into structured fields (see below).
3. Call `handoff_import_context` once with all extracted data.
4. Report the result to the user.

## Field Mapping Guide

### tasks — Extracting Structured Tasks

Every actionable item in the source document becomes a task.
Always populate these fields:

| Field | How to extract |
|---|---|
| `title` | Short imperative phrase (e.g. "Add retry logic to USB reconnect") |
| `status` | Map from source: "done"/"completed" -> `done`, "WIP"/"in progress" -> `in_progress`, "blocked" -> `blocked`, else `todo` |
| `priority` | See priority rules below. Must be `low`, `medium`, or `high` |
| `notes` | Context that doesn't fit elsewhere: root cause, constraints, approach taken |
| `labels` | Category tags from the source (e.g. `["auth", "security"]`, `["pio", "dma"]`) |
| `links` | Related file paths, issue URLs, MR URLs, wiki pages |
| `done_criteria` | Verifiable checklist items extracted from prose (see below) |
| `children` | Sub-tasks nested under a parent |

### done_criteria — Converting Prose to Checkable Items

Transform vague descriptions into specific, testable criteria:

**Before** (raw handoff prose):
> USB reconnect needs to handle the case where the host doesn't re-enumerate.
> Also make sure the pattern engine restarts cleanly.

**After** (structured done_criteria):
```json
[
  {"item": "Host non-enumeration triggers fallback reset after 3s timeout", "checked": false},
  {"item": "Pattern engine SM_RESTART executes on reconnect", "checked": false},
  {"item": "cargo test passes for reconnect scenarios", "checked": false}
]
```

Each criterion should be:
- **Observable**: can be verified by running code, reading output, or checking state
- **Specific**: names the function, file, behavior, or metric
- **Independent**: can be checked without knowing other criteria

### links — What to Reference

```json
[
  "src/usb/reconnect.rs",
  "https://gitlab.com/group/project/-/issues/42",
  "https://gitlab.com/group/project/-/merge_requests/18",
  "wiki/30-usb-protocol.md"
]
```

Include paths to:
- Source files being modified
- Issues or MRs related to the task
- Wiki pages or docs that provide context
- External references (datasheets, specs)

### priority — Estimation Rules

| Priority | When to use |
|---|---|
| `high` | Blocks other work, causes failures, user explicitly flagged as urgent, safety/security |
| `medium` | Improves existing functionality, needed for the current milestone, moderate impact |
| `low` | Nice-to-have, cosmetic, future consideration, no immediate impact |

Signals in source text:
- "must", "critical", "blocker", "breaks", "urgent" -> `high`
- "should", "improve", "enhance", "needed" -> `medium`
- "could", "maybe", "eventually", "nice to have", "low priority" -> `low`

When ambiguous, default to `medium`.

### session — Decisions, Blockers, Notes

| Field | What to extract |
|---|---|
| `decisions` | Technical choices with `reason` and `confidence` (`confirmed`/`estimated`/`unverified`) |
| `blockers` | Anything preventing progress (dependencies, missing info, hardware) |
| `handoff_notes` | `caution`: risks and warnings. `context`: background info. `suggestion`: ideas for improvement |
| `references` | Documents, issues, MRs, wikis relevant to the import |
| `context_pointers` | Files the next session should read first, with line ranges if known |

### raw_notes — The Safety Net

Anything that doesn't fit the structured fields goes into `raw_notes`.
Never discard information from the source — if it can't be structured,
preserve it as raw text.

## Full Example

**Source document** (`tmp/260610-sprint-handoff.md`):
> ## Current work
> Auth module rewrite is 80% done. Decided on OAuth2+PKCE for mobile support.
> Token refresh tests still failing — need to mock the expiry clock.
>
> ## Tasks
> - [x] Session migration script (deployed to prod)
> - [ ] PKCE flow (frontend + backend)
> - [ ] CI pipeline takes 12min, target is 5min
>
> ## Blockers
> - DB migration window not scheduled yet

**Structured call**:
```json
{
  "source": {"description": "tmp/260610-sprint-handoff.md", "format": "markdown"},
  "tasks": [
    {
      "title": "Auth module rewrite",
      "status": "in_progress",
      "priority": "high",
      "notes": "80% done. Token refresh tests failing due to clock mocking.",
      "labels": ["auth", "security"],
      "links": ["src/auth/oauth.rs", "src/auth/token.rs"],
      "done_criteria": [
        {"item": "Token refresh test passes with mocked expiry clock", "checked": false},
        {"item": "OAuth2 PKCE flow works on mobile client", "checked": false}
      ],
      "children": [
        {"title": "Session migration script", "status": "done", "notes": "Deployed to prod"},
        {
          "title": "PKCE flow implementation",
          "status": "in_progress",
          "priority": "high",
          "children": [
            {"title": "Frontend PKCE integration", "status": "todo"},
            {"title": "Backend PKCE endpoints", "status": "todo"}
          ]
        }
      ]
    },
    {
      "title": "CI pipeline optimization",
      "status": "todo",
      "priority": "medium",
      "labels": ["ci"],
      "done_criteria": [
        {"item": "CI build time under 5 minutes", "checked": false}
      ]
    }
  ],
  "session": {
    "summary": "[import] Sprint handoff migration from tmp/260610",
    "decisions": [
      {"decision": "OAuth2 + PKCE for auth", "reason": "Mobile app needs PKCE; implicit flow not viable", "confidence": "confirmed"}
    ],
    "blockers": ["DB migration window not scheduled"],
    "references": [
      {"label": "Source handoff doc", "uri": "tmp/260610-sprint-handoff.md", "type": "doc"}
    ]
  }
}
```

## Common Mistakes

- **Empty done_criteria**: Every `todo`/`in_progress` task should have at least one criterion.
- **Missing links**: If the source mentions files or issues, capture them in `links`.
- **Generic priority**: Don't leave priority empty. Apply the rules above.
- **Flat structure**: If tasks have natural parent-child relationships, use `children`.
- **Discarding info**: Use `raw_notes` for anything that doesn't fit structured fields.
