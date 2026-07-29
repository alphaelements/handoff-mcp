# handoff-mcp repository guidance

## Scope and safety

- Work in the current repository and preserve unrelated user changes.
- Do not push, publish, deploy, or commit directly to `main` or `master`.
- Do not edit `.handoff/` files directly. Use the Handoff MCP tools for task,
  session, document, and memory state.
- Treat write, network, credential, migration, and destructive operations as
  requiring the applicable Codex approval or explicit user authorization.

## Session handoff

This repository uses Handoff MCP for session continuity.

- At session start, call `handoff_load_context`. If it returns
  `session_guidance`, establish an active session with
  `handoff_save_context(session_status: "active")` before work begins.
- Track implementation work with `handoff_update_task`; check each verified
  done criterion with `handoff_check_criterion` before moving a task to `done`.
- At session end, save a concise summary, decisions, blockers, context
  pointers, related task IDs, and concrete next-step suggestions.

## Repository workflow

- Rust commands must source `scripts/cargo-env.sh` first. Use it for builds,
  tests, formatting, clippy, audit, and cargo-deny checks.
- Run focused checks for the files changed, then the relevant release checks:

  ```bash
  . scripts/cargo-env.sh && cargo fmt --all -- --check
  . scripts/cargo-env.sh && cargo clippy --all-targets -- -D warnings
  . scripts/cargo-env.sh && cargo test
  npm run test:js
  ./scripts/sync-workflow-inline.sh --check
  ./scripts/sync-plugin-skills.sh --check
  ./scripts/sync-plugin-version.sh --check
  ```

- When the Codex CLI is installed, also run `npm run test:codex-plugin`. It
  installs the plugin into an empty local Codex profile and verifies the
  installed package contents.

- When changing the package version, keep `package.json`, `Cargo.toml`, the
  Claude plugin manifests, and
  `plugins/handoff-task-loop-codex/.codex-plugin/plugin.json` in sync. The
  existing version-sync script covers the Claude manifests; update the Codex
  manifest deliberately and verify it too.
- When changing shared skills under `skills/`, run
  `./scripts/sync-plugin-skills.sh` so the marketplace-served `plugin/skills/`
  copy remains synchronized.
- Validate a Codex plugin and each changed Codex skill before release. Install
  it from the repository marketplace and verify the installed cache rather
  than only validating source files.

## Claude and Codex boundaries

- Keep Claude Code assets (`.claude-plugin/`, `plugin-task-loop/`,
  `commands/`, `agents/`, and `workflows/`) separate from Codex-only assets
  (`.agents/plugins/` and `plugins/handoff-task-loop-codex/`). Do not make
  Codex features depend on Claude commands or Workflow DSL.
- The Codex Task Loop is explicitly invoked as `$handoff-session-loop`. Its
  Manager uses Handoff MCP and Codex collaboration tools; `/session-loop` and
  `/research-loop` remain Claude Code interfaces.
- Use `AGENTS.md` for durable repository guidance, skills for reusable
  workflows, and MCP only for live Handoff state. Do not duplicate the same
  policy across all three without a concrete need.

## Documentation and release readiness

- Keep public installation instructions accurate for both Claude Code and
  Codex. State which path is recommended and document supported alternatives.
- For release preparation, run `git diff --check`, the applicable validators,
  and the relevant test suite. Report checks that were not run and why.
- Update `CHANGELOG.md` for user-visible behavior, packaging, or installation
  changes.
