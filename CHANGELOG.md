# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.35.1] — 2026-08-25

### Fixed
- **session-loop skill**: split multi-WT steps (1b–1d, 5-wt, 9, 10) into a
  separate `session-loop-multi-wt.md` file, reducing the core skill from 1173
  to 463 lines. The previous size caused hallucinated execution — Claude
  reported "Workflow launched" without calling any tool. Multi-WT file is loaded
  on demand via `Skill(handoff-task-loop:session-loop-multi-wt)` when
  `auto_assign=true`.

### Changed
- **CLAUDE.md template**: reduced to a minimal pointer — declares handoff-mcp
  usage and defers to the `handoff` skill for procedures. The full session
  lifecycle, timer, decisions, spec registration, and memory instructions are
  already in the skill; duplicating them in the template was unnecessary.
- **README**: replaced inline recommended CLAUDE.md code block with a link to
  the template file.

## [0.35.0] — 2026-08-22

### Added
- **Session scope auto-detection**: `SessionData` gains `agent_id`, `worktree`,
  and `scope` fields. Scope (`primary`/`worktree`/`ephemeral`) is auto-detected
  from the git worktree relationship — never a caller parameter. Worktree
  sessions auto-parent to the primary's active session.
- **`handoff_overview` tool**: single-project cross-worktree view showing
  registered agents, a task×agent claim matrix, worktree×branch×session
  mapping, and summary statistics.
- **`handoff_reclaim_task` tool**: admin operation to force-release a lease
  regardless of ownership. Records `task.reclaimed` in `events.jsonl`.
- **Advisory conflict notification**: `TaskData` gains a `scope_paths` field.
  `handoff_claim_task` detects overlapping `scope_paths` with other active
  tasks and returns advisory warnings (`info` for directory containment,
  `warn` for exact file match).
- **`handoff_events` query tool**: filter events by `since`, `task_id`,
  `agent_id`, `event_type`, and `limit`. Additional event types:
  `session.created`, `session.closed`, `agent.registered`.
- **Auto-schedule agent awareness**: `handoff_auto_schedule` response now
  includes `agent_capacity` (claimed vs max-concurrent per agent) and
  `ready_tasks` (dependency-resolved todo tasks sorted by priority).
- **`[worktree.session_loop]` config section**: `auto_assign`,
  `merge_strategy`, `max_concurrent_wts`, `auto_cleanup` settings for
  multi-WT session-loop orchestration, with `serde(default)` backward compat.
- **session-loop multi-WT orchestration** (plugin-task-loop): Steps 1b/1c/1d
  (functional grouping, user approval, WT lifecycle), Step 5-wt (parallel
  subagent execution via `EnterWorktree`), Step 9 (merge orchestration with
  rebase/merge/squash strategies), Step 10 (WT cleanup). Gated on
  `auto_assign = true`; default `false` preserves existing behavior.

### Changed
- `handoff_list_sessions` response includes `scope`, `agent_id`, and
  `worktree` fields when present.
- `handoff_get_task` response includes `scope_paths` when non-empty.
- `handoff_auto_schedule` response includes `agent_capacity` and
  `ready_tasks` sections.

## [0.34.0] — 2026-08-21

### Added
- **Multi-worktree shared storage**: secondary git worktrees automatically
  detect the primary worktree's `.handoff/` and create a symlink to share
  task, session, memory, and document state across all worktrees.
- **Task claim/release with lease-based locking**: `handoff_claim_task` and
  `handoff_release_task` tools provide exclusive task ownership via flock-guarded
  leases (default 30-minute TTL, auto-extended on `handoff_update_task`).
- **Agent registration**: `handoff_load_context` auto-registers agents in
  `.handoff/agents/`, with heartbeat tracking, status classification
  (active/stale/disconnected), and 7-day GC for disconnected agents.
- **`handoff_list_agents` tool**: query registered agents with status filtering
  and optional claimed-task listing.
- **Event log**: `.handoff/events.jsonl` records `task.claimed`, `task.released`,
  and `task.expired` events for audit and debugging.
- **Expired lease auto-recovery**: lazy scan on target operations (load_context,
  claim, release, dashboard, list_tasks) detects expired leases and reverts
  tasks to `todo`.
- **Dashboard agent info**: `handoff_dashboard` shows per-task claim state
  (claimed_by, lease_remaining), stale/expired warnings, and an agents section.
- **`handoff_get_task` lock visibility**: response now includes the `lock` field
  (null when unclaimed).
- **Version marker**: `.handoff/version` written on init, checked on
  load_context with a warning when binary and marker versions differ.
- **Config `[worktree]` section**: optional `handoff_root` override and
  `auto_link` toggle, backward-compatible via `serde(default)`.
- **Advisory warnings**: `handoff_update_task` warns (but does not block) when
  updating a task claimed by a different agent.

### Changed
- **HandlerContext refactor**: all 46 MCP handlers now receive a shared
  `HandlerContext { agent_id, project_dir, handoff_dir }` instead of
  each handler resolving paths independently.
- **`update_task` flock protection**: the read-modify-write cycle in
  `handle_update` is now flock-guarded, preventing lost updates against
  concurrent claim/release operations.

### Fixed
- **Submodule misdetection**: `detect_worktree` now checks
  `--show-superproject-working-tree` to avoid misclassifying git submodules
  as linked worktrees.
- **Agent file collision**: `read_agent` validates the deserialized `agent_id`
  against the requested ID, preventing filename-sanitization collisions.
- **Warning overwrite**: `handoff_load_context` accumulates version-mismatch
  and session-not-found warnings instead of the latter silently replacing
  the former.
- **GIT_DIR interference**: `detect_worktree` and worktree tests clear
  `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` to avoid interference from
  parent git processes (e.g. pre-commit hooks).

## [0.33.0] — 2026-08-11

### Added
- **Cross-lingual semantic memory search**: memory queries now blend BM25 lexical
  scores with semantic cosine similarity from a bundled 607 KB embedding model
  (lexsim 0.8, 585M-pair trained). English memories are retrievable via Japanese
  queries and vice versa when a shared anchor term is present.
- **Hybrid near-duplicate detection**: `memory_save` uses `hybrid_jaccard`
  (lexical + semantic) instead of pure Jaccard for near-duplicate conflict
  detection, and `memory_cleanup` uses pre-computed embeddings for O(N)
  embedding + O(N²) dot-product clustering.
- **Semantic model accessor**: `semantic_model()` provides a process-wide
  `SemanticModelView` via `OnceLock`, parsed once from the `include_bytes!`
  embedded model binary.

### Fixed
- **Document slug contract**: `handoff_doc_save` creation now requires an explicit,
  unique slug in both the tool schema and skill guidance, while updates retain the
  document's stored slug.
- **Session-loop response language**: `/session-loop` now follows an explicit user
  language or infers it from selected task titles and notes, applies it consistently
  to developer, tester, and reviewer roles, and validates safe language names/tags.
- **Node module-type warnings**: the npm wrapper and packaging scripts now use ESM,
  and the root package declares `"type": "module"`, avoiding Node's reparsing warning
  when running ESM workflow and Codex-adapter files directly.

## [0.31.2] — 2026-08-05

### Added
- **Owner-limited rework**: On tester/reviewer failure, only the developer(s)
  whose tasks have findings are re-launched. Previous reports from unaffected
  developers are preserved, eliminating redundant full-team restarts.
- **Stage telemetry**: Workflow results include `stage_telemetry[]` with per-agent
  invocation metadata (round, stage, role, model, effort, verdict, crash status).
- **Enhanced finding schema**: Findings now accept optional `affected_paths`,
  `repair_class`, `verification_commands`, and `doc_impacts` fields for structured
  repair routing.
- **Reviewer micro-fix**: `REVIEW_VERDICT_SCHEMA` accepts `repairs[]` for small
  self-fixes the reviewer applies with test evidence, avoiding a full rework round.
- **Micro-safe boundary enforcement**: `validateMicroSafe()` enforces file count
  limits and forbidden-path patterns (migrations, lockfiles, CI, auth) on reviewer
  self-repairs.
- **Finding closure states**: Four terminal states (`fixed`, `accepted_in_session`,
  `observed_out_of_scope`, `deferred_blocked`) with `classifyFindingClosure()`.
- **Follow-up creation gate**: `canCreateFollowup()` prevents deferring blocking
  findings, enforces a 2-per-session limit, and checks for duplicate tasks.
- **Session repairer agent**: `session-repairer` fixes individual findings with
  targeted verification, scoped to `affected_paths` only.
- **Doc reconciler agent**: `session-doc-reconciler` with audit (read-only) and
  apply (single-writer) modes, `DOC_IMPACT_SCHEMA` / `DOC_UPDATE_SCHEMA`, and
  safety rules preventing spec auto-justification.
- **Baseline fixture**: 64-run reference data (`baseline-fixture.js`) with
  `aggregateByRounds()` and `computeKPIs()` for comparing future optimizations
  against the pre-optimization baseline.
- **Documentation contract template**: CLAUDE.md template includes doc
  source-of-truth mapping, update rules, and AGENTS.md guidance for Codex agents.

### Fixed
- **Profile drift resolved**: `profile.js` documentation and `serialTurnsForProfile()`
  now correctly report `standard` as 2 serial turns (not 3) and `full` as
  `developer → tester → reviewer` (not `integrate ∥ review`), matching the actual
  serial execution in `session-execute.js`.

## [0.31.1] — 2026-08-01

### Fixed
- **Task Loop reviewer/tester no longer raise new categories of findings on
  later review rounds.** The first review round must now surface every
  issue it can find; later rounds only verify prior findings and catch
  problems introduced by rework, so requirements no longer shift on later
  rounds after developers thought they were done.
- **Removed the escalation handoff-write requirement that could leave a
  non-converging session stuck.** If a session still has unresolved review
  findings after the maximum number of rounds, it now completes and files
  one backlog task per unresolved finding (with a linked document describing
  the problem and a recommended fix) instead of requiring a write the
  reviewer could not always perform.

## [0.31.0] — 2026-08-01

### Added
- **`handoff_get_task` accepts an opt-in `include_dependents` flag** that
  populates the response's `dependents` field (`null` when omitted) — tasks
  elsewhere in the project that list this task in their own dependencies,
  each with its own notes and done criteria. Lets a reviewer confirm whether
  a piece of work that looks unconnected is actually wired up by a later,
  already-planned task, instead of guessing from the task's description alone.

### Changed
- **Task Loop reviewer/tester agents no longer flag deferred wiring as a
  defect on faith.** When a piece of a task looks unwired, the agents now
  check for a real downstream task and design doc that both name it as
  intentionally deferred before downgrading the finding — and still treat it
  as a defect if that confirmation is missing. When the staging is legitimate
  but nobody wrote it down, the reviewer records the missing note itself
  instead of only flagging it.

## [0.30.0] — 2026-07-30

### Added
- **Opt-in Codex execution adapter for `/session-loop`** — a standalone
  building block that lets a single agent call in the session-loop Workflow
  be routed through the Codex CLI instead of a Claude subagent, when
  explicitly requested. Not yet wired into the default session-loop path; the
  default remains Claude subagents for every role.

## [0.29.0] — 2026-07-29

### Added
- **Codex-native Handoff Task Loop plugin** — install
  `handoff-task-loop-codex` and explicitly invoke `$handoff-session-loop` to
  coordinate ready Handoff tasks through Codex developer, tester, and reviewer
  stages. The plugin ships role templates, marketplace installation guidance,
  clean-profile installation coverage, and remains independent from Claude
  Code's `/session-loop` implementation.
- **Non-interactive Task Loop guard** — the Codex Task Loop documents the
  Handoff MCP write-approval requirement before a `codex exec` run transitions
  task state.

## [0.28.1] — 2026-07-29

### Fixed
- **`handoff-task-loop`'s `/session-loop` now keeps task state current while a
  session runs**, instead of only at the very end. Tasks move to `in_progress`
  before work starts, `done_criteria` are checked off as developer reports come
  in (even on a failed round), issues discovered during implementation are
  filed as new tasks more reliably, and durable design findings are recorded to
  project docs/memory on the normal success path (previously only on reviewer
  escalation).

## [0.28.0] — 2026-07-25

### Added
- **Codex CLI memory auto-injection hook** — relevant project memories are now
  injected into every Codex turn, matching the behavior the
  `handoff-mcp-hooks` plugin provides in Claude Code. See
  `plugin-hooks/codex/README.md` to install it.

  Codex does not support the `mcp_tool` hook type that the Claude Code hooks
  use, so this ships as a `command` hook that calls `handoff-mcp memory query`.
  Two setup details are easy to miss: the hook path must be **absolute**, and
  Codex **silently skips hooks you have not trusted** — approve it via `/hooks`
  in the TUI, or pass `--dangerously-bypass-hook-trust` for non-interactive
  runs.
- **Agent Plugins standard packaging** — the plugin now ships `plugin.json` and
  `mcp.json` conforming to the [Agent Plugins](https://agent-plugins.org)
  1.0.0 manifest schemas, alongside the existing `.claude-plugin/` manifest.
  Claude Code and Codex CLI installs are unaffected.
- **CLI `--` end-of-options marker** — write `--text -- "$value"` to pass a
  value that itself begins with `--`.

### Fixed
- **CLI values starting with `--` are no longer dropped** — `handoff-mcp memory
  query --text "--force is not working"` previously parsed the text as a flag
  and searched for nothing, returning no results with no error. Any flag value
  beginning with `--` was affected; pass such values after `--`.

## [0.27.0] — 2026-07-25

### Added
- **Prebuilt binaries on npm** — `npm install -g handoff-mcp-server` no longer
  requires a Rust toolchain, a C++ linker, or any compilation. The matching
  binary for your platform is downloaded directly. Prebuilt for Linux, macOS,
  and Windows on both x64 and arm64; see the README "Platform support" table.
  For platforms outside that list (musl/Alpine, FreeBSD, glibc older than
  2.35), use `cargo install handoff-mcp`, or point the npm wrapper at your own
  build with `HANDOFF_MCP_BINARY_PATH`.
- **Windows support** — `npm install -g handoff-mcp-server` now works on
  Windows, which previously failed with `EBADPLATFORM`. Linux, macOS, and WSL
  are unaffected.
- **Codex CLI support** — the plugin now works with both Claude Code and
  Codex CLI. Each skill includes `agents/openai.yaml` with Codex UI metadata
  (`display_name`, `short_description`, `default_prompt`) and an MCP tool
  dependency declaration.
- **`plugin/AGENTS.md`** — session handoff instructions template for Codex
  users. Copy into `~/.codex/AGENTS.md` or a project `AGENTS.md` to enable
  automatic session management behavior.

### Fixed
- **Installs no longer break under npm v12** — npm v12 disables package install
  scripts by default, which left `npm install handoff-mcp-server` reporting
  success while the CLI failed with "binary not found". Installation now runs
  no scripts at all, so nothing is left to be skipped. Installing with
  `--omit=optional` still skips the binary, but now reports the cause instead
  of failing silently.
- **`~/` paths in `config.toml` on Windows** — `scan_dirs` entries starting
  with `~/` were left unexpanded because only the POSIX `HOME` variable was
  consulted, so `handoff_dashboard` and `handoff_refer` silently skipped every
  configured directory. `USERPROFILE` is now used as a fallback, and `~\` is
  accepted alongside `~/`.
- **Concurrent writes to the same `.handoff/` file** — two threads writing
  simultaneously could pick the same temporary filename and corrupt each
  other's data. Temporary names are now unique per write.
- **Transient write failures on Windows** — saving a task or session while the
  VSCode extension had the file open could fail with a permission error. The
  write is now retried briefly.

### Changed
- **Plugin README** updated to document Codex CLI compatibility and
  `AGENTS.md` setup instructions.
- **Lefthook `plugin-skills-sync` glob** now includes `*.yaml` files, so
  `agents/openai.yaml` additions are caught by the pre-commit sync check.

## [0.25.1] — 2026-07-21

### Fixed
- **Source build on platforms without prebuilt binaries** — `templates/`
  directory was missing from the npm package `files` list, causing
  `cargo build` to fail with a missing `include_str!` target on
  platforms like aarch64 Linux where no prebuilt binary is available.

## [0.25.0] — 2026-07-19

### Added
- **Document storage migrated to YAML frontmatter single-file format** —
  metadata and content now live in one `.md` file per document, replacing
  the previous split JSON+content model. Includes automated migration.
- **`doc_update_section` tool** — update a single section of a managed document
  by heading path, with batch `doc_verify` support.
- **E2E migration tests** — comprehensive test suite covering the frontmatter
  migration path and document tool reference in SKILL.md.
- **Verification Matrix v2** — sub-items, freeform items, and checklist output
  for structured review tracking via `handoff_doc_verify`.
- **`suggest_refs` action** — scan project source files and suggest relevant
  cross-references for a document heading.
- **`verify_status` action** — report per-criterion verification progress.
- **`checklist` action** — output a markdown checklist from the verification
  matrix.

### Fixed
- **`read_config` now accepts weekday names in `closed_weekdays`** — VSCode
  extension writes `["sun", "sat"]` but the serde deserializer expected
  `Vec<u32>`. A custom deserializer now normalizes strings, integers, and
  mixed arrays.

## [0.24.10] — 2026-07-18

### Fixed
- **`read_config` now accepts weekday names in `closed_weekdays`** — VSCode
  extension writes `["sun", "sat"]` but the serde deserializer expected
  `Vec<u32>`, breaking all tools that call `read_config` (hooks, load_context,
  save_context, metrics, timer). A custom deserializer now normalizes strings,
  integers, and mixed arrays to `Vec<u32>`.
- **Deduplicated `weekday_to_num`** — consolidated three identical copies
  (capacity.rs, auto_schedule.rs) into a single shared function in
  `storage::config`.

## [0.24.9] — 2026-07-18

### Changed
- **Doc-side keyword TF boost via `build_weighted_tokens`** — keywords now get
  explicit weight 2.0 through `Corpus::build_weighted_tokens` instead of the
  previous 2x text-concatenation hack. Same effective TF contribution, cleaner
  API contract with lexsim.
- **BM25 relevance audit gates raised** — recall@5 gate 0.66 → 0.80, recall@1
  gate 0.50 → 0.55, reflecting lexsim 0.7.0's content-derived CL-CnG trigram
  restoration. Noise ceiling changed from exact 0.0 to ≤ 1.5 (measured max
  1.21, well below production `min_score` 2.0).

### Dependencies
- `lexsim` bumped from `>=0.6.0` to `>=0.7.0` for `Corpus::build_weighted_tokens`
  and content-derived trigram weighting.

## [0.24.8] — 2026-07-18

### Added
- **Relative threshold for memory injection** — new config key
  `memory_query_relative_threshold` (default 0.3). After ranking, candidates
  scoring below `top_score × threshold` are dropped, preventing low-relevance
  "tail noise" from riding a strong top hit.
- **Keywords missing warning** — `memory_query` results include a `warnings`
  array when any injected memory has no `keywords` field, nudging callers to
  add keywords for better match precision.
- **Noise-query false-positive audit** — new `weighted_bm25_noise_query_audit`
  test validates that filler/function-word-only prompts score exactly 0.0 under
  weighted BM25 (plain BM25 scores 5–15 for the same prompts).

### Changed
- **`memory_query_min_score` raised from 0.1 to 2.0** — the previous 0.1 was
  effectively no filter under weighted BM25. Real-corpus analysis (36 memories,
  16 queries) showed unrelated prompts injecting up to 9 memories; at 2.0 the
  false-positive rate drops substantially while all true positives survive
  (lowest measured relevant score: 3.3).
- **Zero-score documents are always excluded** — a BM25 score of exactly 0.0
  means no query term matched; such documents are now unconditionally filtered
  regardless of `min_score`.
- **BM25 relevance audit measures both scorers** — `bm25_relevance_audit` now
  reports plain (baseline) and weighted (production) recall/MRR side by side,
  with the production scorer gated at its measured level (recall@5 ≥ 0.66,
  recall@1 ≥ 0.50). Known gap documented: weighted recall@5 0.71 vs plain 0.88
  due to CL-CnG trigram zeroing; restore gates to 0.82/0.55 when lexsim ships
  content-derived trigram weighting.

### Dependencies
- `lexsim` lock updated 0.6.1 → 0.6.2 (でし stopword fix).

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
