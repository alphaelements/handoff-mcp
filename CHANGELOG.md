# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
