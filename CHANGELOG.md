# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
