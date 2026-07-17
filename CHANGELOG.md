# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.24.7] — 2026-07-17

### Changed
- **Memory and document search now uses weighted BM25** — all BM25 call sites
  (`memory_query`, `doc_query`, `doc_graph`) switched from `lexsim::Corpus::build`
  / `bm25_scores_tokens` to `Corpus::build_weighted` / `bm25_scores_weighted_tokens`.
  Japanese case particles (は/が/を/で/に/…) now boost the content words they
  mark, and stopwords + CL-CnG trigrams are excluded from corpus statistics.
  On the real project memory corpus (35 entries, 13-query eval set), MRR
  improved from 0.923 to 0.936; recall@5 dipped by 1 case (0.08) where five
  topically overlapping mutation-testing memories compete for the top-5 slots.
- **`memory_query_min_score` default lowered from 0.5 to 0.1** — weighted BM25
  scores are typically 0.25–0.4× the plain BM25 scale because stopword and
  trigram contributions are zeroed out. The previous 0.5 floor filtered out
  valid matches in small corpora. Existing `config.toml` overrides are
  unaffected (the default only applies when the key is absent).

### Dependencies
- `lexsim` bumped from `>=0.1.0` to `>=0.6.0` for the weighted BM25 API.

## [0.24.6] — 2026-07-17

### Fixed
- **`handoff_save_context` no longer overwrites accumulated session fields** —
  when updating an active session, fields not explicitly provided (decisions,
  handoff_notes, checklist, references, context_pointers) are now preserved.
  Previously, omitting these fields would silently replace them with empty
  defaults, losing data accumulated via `handoff_update_session`.
- **`handoff_update_session` no longer panics on multibyte text** — the note
  truncation for display messages now slices by character count instead of byte
  position, fixing a crash when `add_handoff_note` received Japanese or other
  multibyte UTF-8 text longer than 60 bytes.

## [0.24.5] — 2026-07-16

### Added
- **`--global` flag for `handoff-mcp setup`** — writes the handoff MCP server
  entry to `~/.claude/settings.json` `mcpServers` instead of the project-local
  `.mcp.json`. Works with `--check`, `--uninstall`, and `--mcp-json`.
- **CLAUDE.md template injection** — `handoff-mcp setup` now appends a
  "Session Handoff" section to the project's `CLAUDE.md` with session lifecycle
  instructions. Skips if the section already exists. Interactive by default;
  `-y` to auto-accept.
- **`--force` flag** — replaces an existing `## Session Handoff` section in
  `CLAUDE.md` with the latest template. Other sections are preserved.
- **`--check` now reports CLAUDE.md status** in addition to hooks and MCP config.

### Changed
- CLAUDE.md template text is loaded from `templates/claude-md-section.md`
  (external file, embedded at compile time via `include_str!`).

## [0.24.4] — 2026-07-16

### Fixed
- **`--mcp-json` no longer installs hooks** — the flag now only touches
  `.mcp.json` as intended, without falling through to the full hook install.

## [0.24.3] — 2026-07-16

### Added
- **`handoff-mcp setup` now configures `.mcp.json`** — adds a `handoff` server
  entry to the project's `.mcp.json` (required for hooks to connect). Interactive
  by default; use `-y` / `--yes` to skip prompts, or `--mcp-json` to only add the
  `.mcp.json` entry without touching hooks. `--check` now reports `.mcp.json`
  status too.

## [0.24.2] — 2026-07-16

### Fixed
- **Hooks plugin: reverted v0.24.1 server name change** — the scoped name
  `plugin:handoff-mcp:handoff` does not work either; Claude Code hooks cannot
  connect to plugin-provided MCP servers at all (platform limitation). Reverted
  hooks back to `"server": "handoff"` (bare name) which works when the project's
  `.mcp.json` defines a `handoff` server entry.

### Changed
- **README: hooks setup instructions** — documented that the hooks plugin
  requires a `handoff` entry in the project `.mcp.json` to function. Without it,
  hooks show "not connected" errors.

## [0.24.1] — 2026-07-16

### Fixed
- **Hooks plugin: MCP server name resolution** — `handoff-mcp-hooks` referenced
  the MCP server as `"handoff"` (bare name), which only resolves when a project
  `.mcp.json` defines that name. Plugin-only installs (npm + marketplace, no
  `.mcp.json`) saw two "not connected" errors on every prompt because the plugin
  server is scoped as `plugin:handoff-mcp:handoff`. All four hook entries now use
  the full scoped name.

## [0.24.0] — 2026-07-13

### Added
- **`append_body` parameter on `handoff_doc_save`** — append new sections to an
  existing document without rewriting the entire body. Includes a `separator`
  parameter (default `"\n\n"`). Mutually exclusive with `body`; requires `doc_id`.
- **Soft h1 heading warning** — `handoff_doc_save` now includes a non-blocking
  warning in `warnings[]` when the saved body does not start with a level-1
  heading (`# ...`). The save itself is never rejected.

### Changed
- **`handoff_doc_save` description rewritten** — now explicitly guides callers to
  save complete documents (one doc = one Markdown file starting with `# Title`),
  group related content into a single document, and use `append_body` for
  incremental additions.
- **`handoff_doc_import` description rewritten** — emphasises that each source
  file becomes one document; callers should not pre-split files.
- **`body` is no longer in the `required` array** of `handoff_doc_save`'s schema
  — validation moved to the handler (either `body` or `append_body` is required).

## [0.23.0] — 2026-07-13

### Added
- **`handoff_task_checklist` tool** — unified readiness view combining a task's
  `done_criteria` with verification progress from linked documents. Two actions:
  `view` shows the combined checklist; `generate` auto-creates `done_criteria`
  from a linked spec's sections.

### Changed
- **`estimate_hours` rule relaxed for `todo` status** — leaf tasks in `todo` no
  longer require `schedule.estimate_hours`. The estimate is enforced when moving
  to `in_progress`, `review`, or `done`. This unblocks the natural parent-first
  creation workflow where the parent is created before its children exist.

## [0.22.1] — 2026-07-13

### Added
- **`handoff_doc_graph` tool** — returns all managed documents as a graph with
  nodes, edges (explicit parent/child and related links, plus implicit
  shared-task and shared-scope connections), layers grouped by `doc_type`, and
  optional per-document verification progress.
- **`handoff_doc_trace` tool** — traces a document's lineage chain up, down, or
  both directions, following related-doc detours, reporting multi-child fork
  branches, and detecting cycles.

## [0.22.0] — 2026-07-12

### Changed
- **session-loop v2: 3-stage serial architecture** — the `session-execute`
  workflow now runs implement → test → review as three sequential stages in a
  single loop, replacing the nested inner-loop + verify-rework-loop structure.
  Scoped testers are removed; a single tester handles both per-task adversarial
  verification and whole-project integration testing. Reduces agent spawns from
  up to 26 (2 tasks, worst case) to 8, and drops `standard` profile from 3 to 2
  serial turns.
- **`test_assignments` arg deprecated** — the workflow ignores it for backward
  compatibility but no longer uses scoped testers. Remove it from your session
  manager calls.
- **`tester_model` arg deprecated** — use `integration_tester_model` instead.
- **`max_review_rounds` arg deprecated** — the single `max_rounds` controls the
  entire loop (implement → test → review = 1 round).

## [0.21.0] — 2026-07-12

### Added
- **`handoff_doc_verify` tool** — attach a verification matrix to any managed
  document. Each section heading becomes a verification item that can be checked,
  skipped, or flagged with notes and code references. Call with `action: "check"`
  to mark items, `action: "sync"` to reconcile items with current section
  headings after a document edit, or `action: "reset"` to clear all checks.
- **`handoff_doc_verify_status` tool** — query the verification state of a
  document: total items, checked/unchecked/skipped counts, and per-item detail.
  Returns a progress summary without modifying state.

### Changed
- **Breaking — document storage migrated to v5 slug-based 2-file layout.**
  Documents are now stored as `_doc.<slug>.json` (metadata) +
  `_frag.<slug>.md` (full markdown body) instead of per-section fragment files.
  Existing v4 documents are migrated automatically on first access — no manual
  action needed. The `doc_id` format is unchanged; only the on-disk layout
  changed.
- `doc_save` now accepts an optional `slug` parameter for human-readable
  filenames. When omitted, the slug is derived from the document title.
- `doc_reassemble` now works against the single-file markdown body, making
  drift detection faster and more reliable.
- `doc_query` corpus cache updated for the new storage layout.

## [0.20.0] — 2026-07-10

### Changed
- **Breaking — `handoff_import_context` now enforces the estimate
  requirement.** When `settings.require_estimate_hours` is on (the default),
  importing a leaf task in status `todo`, `in_progress`, `review`, or `done`
  without a `schedule.estimate_hours` is now rejected, exactly as
  `handoff_update_task` already rejected it. Previously import wrote such tasks
  straight to disk, so a bulk import was a way around the rule. Parent tasks
  (any task with `children`) and the statuses `blocked` and `skipped` remain
  exempt. The whole payload is validated before anything is created, so a
  rejected import writes no tasks at all — including the ones listed before the
  offending entry — and consumes no task IDs. The error names the offending task
  and shows a ready-to-send payload including its `title`, so a caller that
  forgot an estimate can resend in one retry. Imports of historical `done` tasks
  now need an estimate too; set `settings.require_estimate_hours = false` to
  import legacy data without one.
- **Breaking — `handoff_bulk_update_tasks` now enforces the estimate
  requirement.** When `settings.require_estimate_hours` is on (the default), an
  update that would leave a leaf task in status `todo`, `in_progress`, `review`,
  or `done` without a `schedule.estimate_hours` is now rejected, exactly as
  `handoff_update_task` already rejected it. Previously the bulk tool applied
  such an update, so a task could be moved out of `blocked`/`skipped` without
  ever supplying an estimate. Scripts that bulk-change status or dates on
  estimateless tasks will now see those updates fail. Supply
  `schedule.estimate_hours` in the same update to move a task into a status that
  requires one. Parent tasks (any task with children) and the statuses `blocked`
  and `skipped` remain exempt, and a rejection is reported per task in
  `errors[]` — the other updates in the batch still apply, and a rejected task
  is left untouched. The tool description and schema now state the rule, so a
  caller learns it before being rejected rather than after.
- **Breaking — `session-execute` no longer runs the reviewer by default.** The
  workflow takes a new `profile` argument choosing the pipeline depth:
  `express` (developer only, 1 serial agent turn), `standard` (developer →
  tester, 2 turns), or `full` (developer → tester → reviewer, 3 turns — the
  previous behavior). **Omitting `profile` now selects `standard`**, so an
  existing `/session-loop` invocation that passed no profile loses its review
  stage and finishes in two turns instead of three. Pass `profile: 'full'` to
  keep the old pipeline. `express` does not accept `test_assignments`, and an
  unrecognized profile is rejected rather than silently downgraded.
  The workflow result gained `profile` and `stages_run` so callers can tell how
  deep a `passed: true` actually goes; `/session-loop` documents the rules for
  choosing a profile and requires it to be confirmed with you before the run.
  The developer runs the project's quality gates under every profile — `express`
  drops the adversarial layers, not the gates.
- `session-execute` now fetches the session context **once** and injects it into
  every agent's prompt, instead of each developer, tester, and reviewer calling
  `handoff_load_context` themselves to read the same bytes. `/session-loop`
  passes its own step-0 context through the new `context.handoff_context`
  argument (inherited decisions, handoff notes, next actions, and optionally
  pre-fetched memories). Agents still fetch what depends on their own work:
  `handoff_get_task`, `handoff_memory_query`, and — reviewer only —
  `handoff_list_tasks`. **Agents now receive strictly more context than before**:
  `context.prev_session_summary` was previously accepted and then never shown to
  anyone, and `context.design_decisions` reached only the developer.
  A `handoff_load_context` response can be forwarded verbatim — decisions and
  handoff notes nested under `previous_session` are picked up from there, and
  keys the agents cannot use are ignored rather than pasted into the prompt.
- `session-execute` now sets each agent's reasoning effort from the profile
  rather than from a fixed `effort: high` on every agent. The `express`
  developer runs at `medium`; the tester and the reviewer stay at `high`, since
  they are the adversarial layers a deeper profile is paying for.
- On its final review-rework round the reviewer is no longer told both to write
  escalation context to handoff and to never call state-modifying handoff tools.
  The prohibition is lifted for exactly the two escalation writes
  (`handoff_save_context`, `handoff_memory_save`); task and session state remain
  the manager's.
- `session-execute`: `max_rounds` and `max_review_rounds` are now validated.
  A `0`, a negative number, or a non-number is rejected with a clear error.
  Previously `0` silently became the default, and a negative or non-numeric
  value made the loop body never execute — the session returned "not passed"
  having launched no agents at all, with nothing explaining why.
- `handoff_update_task` now advertises that `schedule.estimate_hours` is
  required for leaf tasks. The tool description, the `task` object, and the
  `estimate_hours` field itself all say so, and each names the exemptions
  (parent tasks, and tasks in status `blocked` or `skipped`). Previously the
  requirement was only enforced at call time, so a caller had to be rejected
  once before learning about it.
- When `handoff_update_task` does reject a task for a missing estimate, the
  error now names the offending task by id and title, lists the exemptions,
  and includes a ready-to-send JSON example that can be resent as-is. The
  example matches the rejected call: it carries `title` when creating a task
  and omits it when updating one.
- `handoff_bulk_update_tasks` schedule fields now carry descriptions
  explaining that omitted fields are preserved rather than cleared, and that
  `estimate_hours` takes raw human-effort hours.

### Fixed
- **`handoff_import_context` now rejects circular dependencies.** Previously an
  import could write tasks with self-dependencies or mutual cycles to disk,
  because `validate_dependencies` was never called. The handler now collects
  every task's projected ID and dependencies during the pre-validation pass,
  merges them into the on-disk dependency graph, and checks for cycles in one
  batch — so a cycle that lives entirely inside the payload or spans the payload
  and existing tasks is caught. Legitimate same-payload dependencies (e.g. task
  B depends on task A, both created in one import) continue to work. Dangling
  dependencies (pointing at a task that does not exist) are accepted, matching
  the behavior of `handoff_update_task`.
- **Installing the `handoff-mcp` plugin from the marketplace now delivers its
  skills.** The plugin advertised five skills (`handoff`, `handoff-load`,
  `handoff-memory`, `handoff-refer`, `handoff-import`) but shipped none of
  them, so `/plugin install` registered the MCP server without the skills that
  drive it. `claude plugin details handoff-mcp@handoff-mcp-marketplace` now
  reports `Skills (5)` instead of `Skills (0)`. Existing installs pick the
  skills up on `/plugin update handoff-mcp@handoff-mcp-marketplace` followed by
  a restart. The `handoff-task-loop` and `handoff-mcp-hooks` plugins were never
  affected.
- A `handoff_update_task` create that gets rejected — for a missing estimate,
  an invalid status, a bad priority, or an unknown dependency — no longer
  leaves an empty task directory behind. Previously the rejected task also
  consumed its auto-generated ID, so after two failed creates the next task
  that succeeded was numbered `t3` instead of `t1`.
- `session-execute`: a tester agent that crashed was counted as a pass, so a
  session could be approved with no verification behind it. Crashed, empty,
  and unparseable tester results now all fail the round.
- `session-execute`: the reviewer's own report template contains the line
  `**verdict**: APPROVE | REQUEST_CHANGES`, which the approval check matched —
  a session could self-approve. Testers and the reviewer are now called with a
  structured output schema, so the verdict is a typed field instead of text
  matched against prose.
- `session-execute`: bundled task IDs (`t1+t2`, the syntax documented in
  `/session-loop`) never received rework feedback, because the `+` was
  interpreted as a regular-expression quantifier. Task IDs are now escaped, and
  are matched whole so `t1` no longer steals the findings reported for `t12`.
- `session-execute`: a task that failed one round and passed the next kept
  receiving the old feedback — on the round it passed, its own passing report
  was fed back as "previous feedback" to fix. Rework notes are now re-derived
  each round and cleared for tasks that pass.
- `session-execute`: when a tester reports an overall failure without naming
  which task failed, every task now receives that failure text. Previously the
  rework round re-ran the developers with no feedback at all.
- `session-execute`: a session whose developer agent crashed reported
  `passed: true` despite no work having been done. A developer that returns no
  report now fails the session under every profile.

## [0.19.1] - 2026-07-08

### Changed
- Upgraded the lexical similarity engine to lexsim 0.4.0. Japanese memory
  text now segments into real words instead of character bigrams, improving
  `memory_query` relevance ranking and near-duplicate detection for
  Japanese-language memories.

## [0.19.0] - 2026-07-07

### Changed
- `session-execute` / `research-execute` workflows now validate required
  parameters (`tasks`, `facets`, etc.) at startup and throw a clear error
  message when any are missing. The message specifically notes that
  `resumeFromRunId` does not auto-inherit `args` from the previous run.
  `/session-loop` and `/research-loop` documentation updated with a
  resume-warning note.

### Fixed
- Removed the synchronous `handoff_memory_cleanup` hook from `SessionStart`
  — this was the confirmed trigger for VSCode hangs when many parallel
  sub-agents fired cleanup requests at the single-threaded stdio server
  simultaneously. `memory_cleanup` remains available for manual / CLI use.
- `handoff-mcp setup` now auto-detects and removes the legacy `SessionStart`
  cleanup hook from existing installs. `setup --check` warns if one is found.
- Added a per-request timeout (30s, configurable via
  `HANDOFF_MCP_REQUEST_TIMEOUT_SECS`) to the stdio server loop. On timeout,
  the server returns a JSON-RPC error (`-32603`) instead of hanging
  indefinitely.

## [0.18.7] - 2026-07-06

### Fixed
- `/session-loop` and `/research-loop` now correctly invoke their workflows
  with the `handoff-task-loop:` namespace prefix, fixing "Workflow not found"
  errors at runtime.

## [0.18.6] - 2026-07-06

### Fixed
- `handoff-mcp-hooks` plugin: removed `project_dir` and `session_id` from
  hook inputs — these `${...}` placeholders are not expanded in plugin
  `mcp_tool` hook inputs. The MCP server now reads `CLAUDE_PROJECT_DIR`
  from its process environment (set by Claude Code at server startup),
  so hooks no longer need to pass the project path explicitly.

## [0.18.5] - 2026-07-06

### Fixed
- `resolve_project_dir` now reads the `CLAUDE_PROJECT_DIR` environment variable
  (set by Claude Code on MCP server processes) as a fallback when the
  `project_dir` argument is missing or unexpanded. Fallback chain:
  `project_dir` argument → `CLAUDE_PROJECT_DIR` env var → current directory.
  Fixes hook-triggered `Invalid project path` errors when `${CLAUDE_PROJECT_DIR}`
  was not expanded in `mcp_tool` hook inputs.
- `handoff-mcp-hooks` plugin: removed `hooks` field from `plugin.json`
  (Claude Code auto-loads `hooks/hooks.json`; the explicit reference caused a
  duplicate-load error). Added proper wrapper structure and `matcher` fields
  to `hooks.json`.

## [0.18.4] - 2026-07-06

### Fixed
- `handoff-mcp-hooks` plugin hooks are now auto-loaded from `hooks/hooks.json`
  (the standard plugin convention). Removed the explicit `hooks` field from
  `plugin.json` which caused a duplicate-load error. Added proper wrapper
  structure (`{ "hooks": { ... } }`) and `matcher` fields to `hooks.json`.

## [0.18.3] - 2026-07-06

### Fixed
- `handoff-mcp-hooks` plugin still failed to load after the inline fix in
  0.18.2. Root cause: plugin hooks require an explicit `matcher` field on
  every matcher group (`"matcher": "*"` for catch-all). Added `matcher` to
  `UserPromptSubmit` and `SessionStart` entries.

## [0.18.2] - 2026-07-06

### Fixed
- `handoff-mcp-hooks` plugin failed to load because `plugin.json` referenced
  hooks via a file path string (`"./hooks/hooks.json"`). Claude Code expects
  the `hooks` field to be an inline object. Hooks are now inlined directly
  in `plugin.json`.

## [0.18.1] - 2026-07-06

### Added
- **Research loop** (`/research-loop`): New multi-agent workflow for technical
  investigation and specification authoring. Parallel investigators explore
  research facets, adversarial verifiers cross-check findings, an Opus-level
  director gates quality, and a drafter synthesizes verified evidence into
  specifications, technical reports, or decision documents. Includes iterative
  re-investigation (max 2 rounds) and revision loops with convergence
  obligations. New agents: `research-investigator`, `research-verifier`,
  `research-drafter`, `research-director`. New workflow: `research-execute`.

## [0.18.0] - 2026-07-06

### Added
- `handoff_dashboard` now scans `scan_dirs` recursively (previously only the
  immediate children were scanned), discovering nested `.handoff/` projects
  at any depth. New optional `max_depth` argument (defaults to the config's
  `dashboard.max_depth`, itself defaulting to `5`) caps the recursion depth;
  `exclude_patterns` (directory-name exact match) skips whole subtrees, e.g.
  `["node_modules"]`. Existing single-level scans are unaffected.
- `handoff_list_tasks` accepts `include_children` (default `false`). When
  `true`, it recursively scans `project_dir` for nested `.handoff/` child
  projects (e.g. sub-packages in a monorepo) and merges their tasks into the
  response. Each task in the merged tree gains `project_name`, `project_dir`,
  and `task_ref` fields; `task_ref` is a composite identifier
  (`{project_name}-{hash}:{id}`) unique across projects for display purposes.
  The original `id` is left unchanged so it stays directly usable with
  `handoff_get_task` / `handoff_update_task` (paired with the task's
  `project_dir`) and so `dependencies` entries keep resolving correctly.
- `handoff_load_context` now always returns a `child_projects` array
  describing nested `.handoff/` projects discovered under `project_dir`
  (empty array when there are none). Each entry includes `name`, `dir`,
  `task_count`, and `status_summary`.

## [0.17.3] - 2026-07-05

### Fixed
- Task-loop workflow agents failed to launch with "agent type not found"
  error. Plugin-scoped agent types now use fully qualified names
  (`handoff-task-loop:session-developer`, etc.).

## [0.17.2] - 2026-07-04

### Changed
- Task-loop workflow: review now runs once after all tests pass instead of
  every rework round. Test failures trigger an inner rework loop (up to 3
  rounds); the reviewer only sees the final result. If the reviewer requests
  changes, up to 2 review-rework rounds run before escalating unresolved
  issues to the handoff session context for the next session.
- Task-loop agents (developer, tester) can now read handoff context
  (previous session decisions, project memory) for better cross-session
  awareness. The reviewer can additionally write escalation context when
  review rework is exhausted.
- Task-loop model defaults: developers and testers always use Sonnet.
  Removed automatic Opus upgrade for high-complexity tasks.

### Fixed
- `project_dir` parameter now falls back to the current working directory
  when it arrives as an empty string or an unexpanded template variable
  (e.g. `${CLAUDE_PROJECT_DIR}`). Previously this caused an "Invalid
  project path:" error in hook-triggered tool calls.

## [0.17.1] - 2026-07-04

### Fixed
- Forked sessions (`handoff_fork_session`) were immediately deleted by
  `enforce_history_limit` when closed via `handoff_save_context`. The
  session file was written with an all-zeros timestamp prefix, causing it
  to sort as the oldest entry and be pruned on the next close.
- Memory auto-injection hooks used `${cwd}` for the project directory,
  which broke when Claude Code changed its working directory. Now uses
  `${CLAUDE_PROJECT_DIR}` (stable project root). Run `handoff-mcp setup`
  to update existing hook installations.

## [0.17.0] - 2026-07-04

### Added
- Multi-session support: multiple active sessions can coexist in a single
  project (`multi_session = true`, default for new projects).
  - `handoff_fork_session`: fork a new session from an existing one,
    inheriting decisions, context_pointers, references, and handoff_notes.
    Sets `parent_session_id` for timeline tracking.
  - `handoff_merge_sessions`: merge multiple sessions into one, combining
    decisions and notes with duplicate-decision conflict detection.
  - `handoff_list_sessions`: new `timeline` filter and `include_children`
    option for visualizing session branching.
  - `handoff_load_context` / `handoff_save_context`: `session_id` parameter
    for targeting a specific active session; `timeline`, `label`, and
    `related_task_ids` fields on sessions.
  - Session switch via `pause_session_id` + `load_context(session_id)`.
- `notes_append` parameter on `handoff_update_task` and
  `handoff_bulk_update_tasks`: append text to existing task notes with a
  server-generated timestamp heading, avoiding the read-modify-write
  pattern that risks history loss. `notes` (replace) takes precedence
  when both are provided.

## [0.16.0] - 2026-07-02

### Added
- Claude Code plugin distribution: install handoff-mcp with `/plugin install`
  instead of manual MCP registration. The plugin bundles the MCP server
  definition and all 5 skills (handoff, handoff-load, handoff-memory,
  handoff-refer, handoff-import).
- Optional `handoff-mcp-hooks` plugin for automatic memory injection
  (disabled by default). Install and enable separately to inject project
  memories on every prompt and file edit. Disable anytime with
  `/plugin disable handoff-mcp-hooks`.
- Marketplace support: add `alphaelements/handoff-mcp` as a Claude Code
  marketplace to discover and install both plugins.

## [0.15.1] - 2026-07-01

### Added
- `skills/handoff-memory/` skill: usage guide for the memory tools
  (`handoff_memory_save`, `handoff_memory_query`, `handoff_memory_delete`,
  `handoff_memory_cleanup`) covering save arguments, near-duplicate conflict
  handling, cleanup procedures, scope_paths best practices, and automatic
  injection hooks.

## [0.15.0] - 2026-06-30

### Added
- CLI API: all 37 MCP tools are now callable as shell commands via
  `handoff-mcp <group> <action> [--key value ...]`. Groups: `init`, `task`,
  `session`, `config`, `memory`, `referral`, `assignee`, `milestone`,
  `calendar`, `labels`, `project`, `metrics`, `capacity`, `schedule`,
  `dashboard`, `timer`. All output is JSON on stdout for programmatic
  consumption (e.g. `child_process.execFile` from a VSCode extension).
- Per-group `--help` (e.g. `handoff-mcp memory --help`) shows available
  actions and their flags.
- `--project-dir` global option works across all CLI commands.

## [0.14.1] - 2026-06-29

### Fixed
- Task ID resolution now works correctly for IDs containing hyphens
  (e.g. `m2-burst`, `feat-login`). Previously, `handoff_update_task`,
  `handoff_get_task`, `handoff_check_criterion`, `handoff_log_time`, and
  timer tools could not find tasks whose IDs contained hyphens, returning
  "does not exist" even though `handoff_list_tasks` listed them correctly.

### Changed
- "Task not found" errors now suggest similar task IDs when available,
  helping you correct typos without needing a separate `handoff_list_tasks`
  call.

## [0.14.0] - 2026-06-28

### Added
- Timer coordination tools: `handoff_timer_start`, `handoff_timer_stop`,
  `handoff_timer_get_time`. When the VSCode extension is running, timer
  operations are delegated to it via `.handoff/timer/requests/`. When the
  extension is absent, MCP runs a fallback internal timer and logs elapsed
  hours to `actual_hours` on stop.
- Timer config settings: `timer_provider` (`auto`/`vscode`/`mcp`/`off`),
  `timer_authority_ttl_secs` (heartbeat freshness TTL, default 30),
  `timer_idle_timeout_minutes` (fallback idle threshold, default 10).

## [0.13.1] - 2026-06-28

### Added
- `handoff-mcp setup` command — automatically installs Claude Code hooks for
  memory auto-injection into `~/.claude/settings.json`. No manual JSON editing
  needed. Subcommands: `--check` (show status), `--uninstall` (remove hooks).

### Changed
- Memory tool names now use the `handoff_` prefix for consistency with all other
  tools: `handoff_memory_save`, `handoff_memory_query`, `handoff_memory_delete`,
  `handoff_memory_cleanup` (previously `memory_save`, `memory_query`, etc.).

### Fixed
- Settings file writes are now atomic (temp-file + rename) to prevent corruption
  on crash.
- `serde_json` `preserve_order` feature enabled so `settings.json` key order is
  preserved across reads and writes.

## [0.13.0] - 2026-06-27

### Added
- Project memory: a per-project store of durable lessons that the AI can carry
  across sessions, with a multilingual (Japanese / English) similarity engine
  for de-duplication and relevance ranking. New tools:
  - `memory_save` — persist a memory (`text`, optional `kind`, `tags`,
    `scope_paths`). Exact duplicates are not rewritten; a near-duplicate is
    returned as a `conflict` with both bodies so the AI can merge it via
    `merge_into` (pass `force` to save it separately anyway).
  - `memory_query` — return the memories most relevant to the current prompt
    and/or edited files, ranked by relevance with a boost for memories scoped to
    the file being edited. When a `session_id` is supplied, a memory already
    surfaced this session is not repeated until it changes.
  - `memory_delete` — remove a memory by ID (or unique ID prefix).
  - `memory_cleanup` — housekeep the store (intended to run at session start).
    Silently merges exact-duplicate memories (lossless — the survivor inherits
    the union of the absorbed memories' tags, scope paths, and supersession
    history, the sum of their hit counts, and the latest reference time), then
    returns recommendations to act on: near-duplicate clusters (merge with
    `memory_save merge_into=…`) and stale memories not referenced for
    `stale_days` (consider `memory_delete`). Also garbage-collects old
    per-session injection sidecars. Parameters: `apply_exact_merges`
    (default true), `stale_days` (default 60).
- New settings (all settable via `handoff_update_config`, all with safe
  defaults so existing projects need no change):
  - `settings.memory_enabled` (default true) — master switch; when false, all
    four memory tools return a benign empty (disabled) result and write nothing.
  - `settings.memory_dup_threshold` (default 0.72) — similarity at/above which
    `memory_save` treats a save as a near-duplicate conflict, and `memory_cleanup`
    groups a near-duplicate cluster.
  - `settings.memory_query_min_score` (default 0.5) — relevance floor below which
    `memory_query` does not return a memory.
  - `settings.memory_query_limit` (default 5) — maximum memories per query.
  - `settings.memory_stale_days` (default 60) — age at which `memory_cleanup`
    flags an unreferenced memory as stale.
  - `settings.memory_injected_gc_days` (default 14) — age at which `memory_cleanup`
    garbage-collects a per-session injection record.

## [0.12.0] - 2026-06-27

### Added
- `handoff_get_referral` tool: fetch the full body of a single incoming referral
  by ID (or unique ID prefix) — summary, details, suggested tasks with their
  done_criteria, priority, context, and status. Previously `handoff_list_referrals`
  returned only summaries, so referral details could not be read through the MCP.
- `handoff_get_metrics` now reports `ai_estimate_multiplier`,
  `total_adjusted_estimate_hours`, and a per-milestone `adjusted_estimate_hours`.
  The adjusted estimate is the raw human-effort estimate multiplied by the
  configurable AI-effort multiplier (default 0.2); raw estimates are unchanged.
- New settings: `settings.require_estimate_hours` (default true) and
  `settings.ai_estimate_multiplier` (default 0.2), settable via
  `handoff_update_config`.

### Changed
- `handoff_update_task` now requires `schedule.estimate_hours` (> 0) when
  creating or updating a leaf task in a non-`blocked`/`skipped` status. Parent
  tasks (with children) are exempt, and an estimate already on the task satisfies
  the requirement. Set `settings.require_estimate_hours = false` to opt out.
- `handoff_get_capacity` allocates AI-effort hours: a task's raw estimate is
  multiplied by `ai_estimate_multiplier` when distributing it across days
  (`remaining_hours`, being actual progress, is used as-is).

## [0.11.0] - 2026-06-24

GUI-MCP parity with the handoff-vscode v0.5 extension: every config.toml section
the GUI writes is now a typed model with dedicated MCP CRUD tools, and writes are
crash-safe.

### Added

- Team CRUD tools: `handoff_add_assignee`, `handoff_update_assignee`, `handoff_remove_assignee` (removal also unassigns the member from every task)
- Milestone CRUD tools: `handoff_list_milestones`, `handoff_add_milestone`, `handoff_update_milestone`, `handoff_remove_milestone`
- Project tools: `handoff_update_calendar` (work hours, closed days, `day_hours`, schedule_mode), `handoff_update_labels`, `handoff_start_project` (sets `started_at`, optionally shifts all task dates)
- `Config` model now covers `started_at`, `schedule_mode`, top-level `labels`, `[calendar]`, `[assignees.*]`, `[milestones.*]`, `[gantt_view]`, and `[effort_budget]` (all `serde(default)` for backward compatibility)
- `handoff_auto_schedule` records an applied-changes decision on the active session and returns the assignee capacity / calendar conditions it used; added an optional `start_date` anchor

### Changed

- `handoff_update_task`'s `schedule` field now **merges** instead of replacing: partial updates (e.g. milestone only) no longer wipe `actual_hours` / `remaining_hours`
- `handoff_auto_schedule` honors per-day capacity overrides (`calendar.day_hours`), e.g. a half-day Friday extends a task

### Fixed

- All handoff writes (tasks, config, sessions, referrals) are now atomic (temp file + fsync + rename), so a concurrent reader never sees a partially written file
- Task writes use optimistic concurrency control (`updated_at` check + retry), preventing lost updates when the VSCode extension writes the same task

## [0.8.0] - 2026-06-18

### Added

- Upsert mode for `handoff_update_task`: specifying a non-existent ID now creates a new task with that exact ID, enabling batch creation with pre-defined dependencies

### Changed

- All "Task not found" errors now include actionable guidance (suggesting `handoff_list_tasks` to discover valid IDs). Affected tools: `handoff_update_task`, `handoff_get_task`, `handoff_check_criterion`
- Updated `id` field description in `handoff_update_task` schema to document upsert behavior

## [0.7.3] - 2026-06-17

### Added

- `session_status` parameter on `save_context` to preserve open sessions across saves
- Session `paused` status for temporary context switching between projects

### Fixed

- npm postinstall script cross-platform compatibility; exclude prebuilt binary from package
- Active session uniqueness enforcement to prevent cross-session close conflicts

## [0.6.2] - 2026-06-15

### Fixed

- Prebuilt binary updated to match version 0.6.2

## [0.6.1] - 2026-06-15

### Added

- Spec document reference validation and path resolution in `handoff_refer`
- Validation warnings on `handoff_refer` for malformed or missing targets

## [0.6.0] - 2026-06-15

### Added

- Session IDs for targeted close and activate operations
- Warnings when `session_id` or `close_session_id` not found

### Changed

- Reduced duplicate notes across sessions

## [0.5.0] - 2026-06-15

### Added

- `next_actions` field in `load_context` response for recommended next steps
- Open / active / closed session lifecycle management
- Soft validation warnings on `save_context` for incomplete or inconsistent data
- Enriched schema descriptions across all MCP tools

### Fixed

- Next session no longer re-verifies work already completed by the previous session

## [0.4.0] - 2026-06-14

### Added

- `schedule`, `dependencies`, and `order` fields on the task model for sequencing and planning

## [0.3.0] - 2026-06-13

### Added

- `handoff_get_task` tool for retrieving a single task by ID
- `handoff_check_criterion` tool for marking individual done-criteria
- Cross-project referrals via `handoff_refer`
- Priority validation on task creation and updates

## [0.2.0] - 2026-06-13

### Added

- `handoff_import_context` tool for bulk import from handoff documents
- Unicode slug support for task IDs
- npm distribution as `handoff-mcp-server` package

### Fixed

- `.mcp.json` format corrected to use `mcpServers` wrapper with `type` field

## [0.1.0] - 2026-06-13

### Added

- Initial MCP server implementation with stdio transport
- Core tools: `handoff_init`, `handoff_load_context`, `handoff_save_context`
- Task management: `handoff_list_tasks`, `handoff_update_task`
- Configuration: `handoff_get_config`, `handoff_update_config`
- Cross-project dashboard: `handoff_dashboard`
- `.handoff/` directory-based persistence

[Unreleased]: https://github.com/alphaelements/handoff-mcp/compare/v0.8.0...HEAD
[0.8.0]: https://github.com/alphaelements/handoff-mcp/compare/v0.7.3...v0.8.0
[0.7.3]: https://github.com/alphaelements/handoff-mcp/compare/v0.6.2...v0.7.3
[0.6.2]: https://github.com/alphaelements/handoff-mcp/compare/v0.6.1...v0.6.2
[0.6.1]: https://github.com/alphaelements/handoff-mcp/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/alphaelements/handoff-mcp/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/alphaelements/handoff-mcp/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/alphaelements/handoff-mcp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/alphaelements/handoff-mcp/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/alphaelements/handoff-mcp/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/alphaelements/handoff-mcp/releases/tag/v0.1.0
