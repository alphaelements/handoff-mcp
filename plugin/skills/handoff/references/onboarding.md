# Next step, readiness check, release check, project commands, the beginner preset and `[onboarding]`

How to tell what to do next in a project (`handoff_next_step`), whether it is
ready for agent-driven work (`handoff_doctor`), whether a milestone is ready to
ship (`handoff_release_check`), and the settings that get a project ready and
switch on guidance for first-time users.
The settings are optional and off by default: a project that sets none of them
behaves as before.

## `handoff_next_step` — what to do next

`handoff_next_step()` looks at the project as a whole and answers with its
state and **one** recommended operation:

```
handoff_next_step()
→ { state, action: { title, command, reason }, facts }
```

The states are checked in this order; the first that applies is the answer.

| # | `state` | Applies when | `action.command` |
|---|---------|--------------|------------------|
| 1 | `not_initialized` | the project has no `.handoff/` | `handoff_init(project_name: "...")` |
| 2 | `not_ready` | `handoff_doctor()` (without results) is not ready | what its first failed check says to do (a sentence) |
| 3 | `review_pending` | a task is in `review` | `handoff_get_task(task_id: "...")`: confirm it with the user |
| 4 | `in_progress` | a task is `in_progress`, or a session is active | `handoff_get_task(...)`, or `handoff_get_session(...)` when only a session is active |
| 5 | `no_brainstorm` | no finalized brainstorm report and no requirement item | `/brainstorm-loop` |
| 6 | `no_requirements` | the requirement layer has no item | `/requirement-loop` |
| 7 | `ready_tasks` | a `todo` task whose dependencies are all done or skipped | `/session-loop` |
| 8 | `release_ready` | no unfinished task, and a milestone that is neither released nor cancelled | `handoff_release_check(milestone: "...")` |
| 9 | `all_done` | none of the above | `/brainstorm-loop` to plan what comes next; `handoff_list_tasks()` when unfinished tasks remain that cannot start (blocked, or waiting for a dependency) |

How to use the answer:

- Tell the user the `action.title` and `action.reason`, then carry out
  `action.command`: run the slash command, call the tool, or act on the
  sentence. Do one thing; call `handoff_next_step` again afterwards.
- The order is fixed. A project with ready tasks but no requirement item is
  `no_brainstorm` or `no_requirements`, not `ready_tasks`; a task in review
  comes before everything except initialization and readiness.
- `facts` holds the counts the decision looked at: `doctor: {ready, failed}`,
  `tasks: {total, by_status, unfinished, ready}`, `active_sessions`,
  `brainstorm_reports`, `requirement_items`, `milestones: {total, unreleased}`.
  A group the decision did not need is left out (a project that is not ready
  reports no task counts).
- The tool is read-only: it writes nothing, runs no command and starts no
  process, and it works before `handoff_init`. Calling it twice gives the same
  answer.
- With `onboarding.next_step = true`, `handoff_load_context` carries the same
  answer as its `next_step` key, so no extra call is needed at session start.
  If it cannot be worked out, the context still loads and `warnings` says why.
  Without the setting the key is absent.
- It is not `handoff_trace_next`: that tool lists the verification items of the
  trace graph that still need attention. `handoff_next_step` decides what the
  project needs as a whole.

In a terminal: `handoff-mcp next-step` prints the same JSON.

## `handoff_doctor` — is the project ready?

`handoff_doctor()` reports, item by item, whether the task loop can run here.
It is read-only and works on a project that is not initialized yet.

```
handoff_doctor()
→ { ready, checks: [{id, status, detail, next_step?, command?, exit_code?,
                     duration_ms?, source?}],
    commands_to_run: [{id, command, timeout_sec}], warnings }
```

| Check | Passes when |
|-------|-------------|
| `handoff_initialized` | `.handoff/config.toml` exists and can be read |
| `mcp_registered` | a handoff server entry is found (`.mcp.json`, `~/.claude/settings.json`, `~/.claude.json`, the Claude Code plugin list, `~/.codex/config.toml`); `skip` when none is found, because other clients are not inspected |
| `claude_md_sections` | `CLAUDE.md` has the Build & Test, Coding Rules, Project Structure and Documentation Contract sections |
| `claude_md_todo` | `CLAUDE.md` holds no `TODO(handoff)` marker |
| `commands_defined` | `[commands]` defines at least one command (and `test`, once results are reported) |
| `cmd_install` / `cmd_build` / `cmd_test` / `cmd_lint` / `cmd_typecheck` | the reported result of that command has exit code 0 |
| `smoke_defined` | `[commands] smoke` is defined (`skip` when it is not; never a failure) |
| `secrets_untracked` | Git tracks no file whose name matches a secrets pattern (`skip` outside a Git repository) — see [Secrets files and the dependency audit](#secrets-files-and-the-dependency-audit) |
| `dependencies_audited` | the reported result of `[commands] audit` has exit code 0 (`skip` when it is not defined or not reported) |

`status` is `pass`, `fail`, `skip` or `manual`. `skip` means the check was not
carried out (no result was given, or nothing is defined); it is never a pass.
Every `fail` carries a `next_step` that says what to do. `ready` is true when
nothing failed; a `skip` does not lower it.

**The tool never runs a command.** Running them is your job:

1. Call `handoff_doctor()`. Every `cmd_*` check and `dependencies_audited` is
   `skip`, and `commands_to_run` lists the commands that are defined.
2. Run each command yourself, from the project root, through the shell, exactly
   as given. Stop it after `timeout_sec` (`[onboarding] command_timeout_sec`,
   default 600).
3. Call again with what happened:

   ```
   handoff_doctor(results: [
     { id: "cmd_test", command: "npm run test", exit_code: 0, duration_ms: 8200 },
     { id: "cmd_build", command: "npm run build", exit_code: 2,
       output_tail: "<last ~20 lines of output>" },
     { id: "cmd_lint", command: "npm run lint", error: "timed out after 600s" }
   ])
   ```

   - `exit_code` 0 passes, anything else fails. Use `error` instead when the
     command could not be started or was stopped at the timeout; it fails too.
     Leave `error` out when the command ran to completion.
   - A result whose `command` is not the current `[commands]` value is not used
     and fails the check: report the command you were given, unchanged.
   - Results carry `source: "reported"` in the answer.
   - Once results are reported, a project without `[commands] test` is not
     ready: a run cannot show the project works without its tests.
4. Show the user each failed check with its `next_step`.

`checks: ["cmd_test", ...]` limits the diagnosis to the named checks.
`commands_to_run` holds `install`, `build`, `test`, `lint`, `typecheck` and
`audit` (id `dependencies_audited`) when they are defined; `format` and `smoke`
are never part of it.

A call the tool cannot take at its word is an error and judges nothing: an
argument or result field it does not know (a misspelled `results` is not
silently dropped), an unknown or empty `checks`, a result for something that is
not a command check (`cmd_*`, `dependencies_audited`) or not among the requested
`checks`, two results for one check, a result with neither `exit_code` nor
`error`, or an empty `error`.

In a terminal the user gets the same diagnosis with `handoff-mcp setup --check`
(`--run` makes the CLI run the commands itself, with `source: "executed"`;
`--json` prints the JSON above; the exit code is 1 when the project is not
ready).

## Secrets files and the dependency audit

Two checks appear in both `handoff_doctor` and `handoff_release_check`.

**`secrets_untracked`** — does Git track a file that usually holds secrets?

- It fails when a tracked file's name matches `.env`, `.env.*`, `*.pem`,
  `*.key`, `id_rsa`, `id_ed25519`, `credentials.json` or `secrets.*`, in any
  directory. `.env.example`, `.env.sample` and `.env.template` are never
  reported. `detail` lists the matching paths.
- **Only file names are matched; file contents are not scanned.** A `pass` does
  not mean the repository holds no secret. Say so when you report it.
- It is `skip` (never a pass) outside a Git repository, and when the Git index
  cannot be read completely (a split or sparse index).
- The tracked files are read from the Git index file. Neither tool starts
  `git`.
- On a `fail`: the user stops tracking the file (`git rm --cached <path>`),
  adds it to `.gitignore`, and replaces any secret that was already committed
  or pushed. Removing the file does not remove it from the history.
- `onboarding.secret_file_patterns` replaces the patterns:
  `handoff_update_config(updates: {"onboarding.secret_file_patterns": ["*.tfstate", ".env"]})`.
  `*` matches any run of characters, `?` one; letter case is ignored. A pattern
  without `/` applies to the file name in every directory, one with `/` to the
  whole path. An empty list makes the check `skip`.

**`dependencies_audited`** — the result of `[commands] audit` (for example
`npm audit`, `cargo audit`, `pip-audit`).

- The command is handed out in `commands_to_run` and judged from the result you
  report, like every other command. `detail` quotes the end of its output (up
  to 20 lines), also when it passes.
- It is `skip` when `[commands] audit` is not defined or no result was given.
  Do not report it as passed in that case.

## `handoff_release_check` — is the milestone ready to ship?

`handoff_release_check(milestone: "<name>")` lists, item by item, what stands
between a milestone and its release.

```
handoff_release_check(milestone: "v1.0")
→ { milestone, ok,
    items: [{id, status, detail, refs?, source?, checked_at?}],
    commands_to_run: [{id, command, timeout_sec}] }
```

| Item | Passes when |
|------|-------------|
| `tasks_done` | every leaf task of the milestone is `done` or `skipped`; `refs` = the unfinished task ids. A subtask without a milestone of its own belongs to its parent's |
| `no_open_bugs` | no task of the milestone labelled `bug` is open; `refs` = the open ones |
| `requirements_verified` | every requirement-layer item planned for the milestone is `passing`; `refs` = the others. `skip` when the milestone has no requirement item |
| `tests_pass` | the reported result of `[commands] test` has exit code 0 |
| `smoke_pass` | the reported result of `[commands] smoke` has exit code 0 |
| `secrets_untracked` | see above |
| `dependencies_audited` | see above |
| `manual:<name>` | a person confirmed it — one item per entry of `release.manual_checks`; `manual` until then |

`ok` is true **only when every item is `pass`**. A `skip` (not carried out) or
a `manual` (waiting for a person) keeps it false: a milestone without
requirement items, or a project without a smoke or audit command, does not get
`ok: true`. Report such items as not checked, not as passed.

**The tool never runs a command.** The procedure:

1. Call `handoff_release_check(milestone: "v1.0")`. The command items are
   `skip`; `commands_to_run` lists the commands that are defined.
2. Run each command yourself (project root, through the shell, stop after
   `timeout_sec`) and call again:

   ```
   handoff_release_check(milestone: "v1.0", results: [
     { id: "tests_pass", command: "npm run test", exit_code: 0, duration_ms: 8200 },
     { id: "smoke_pass", command: "./scripts/smoke.sh", exit_code: 0 },
     { id: "dependencies_audited", command: "npm audit", exit_code: 1,
       output_tail: "<last ~20 lines of output>" }
   ])
   ```

   The rules for a result are the same as for `handoff_doctor` (`error` instead
   of `exit_code` for a command that did not finish; a result for a command
   other than the current `[commands]` value is not used and fails the item).
3. For each `manual` item, ask the user. Only after they confirm, call
   `handoff_release_check(milestone: "v1.0", confirm: ["manual:docs_updated"])`.
   Never confirm a manual item on your own judgment.
4. Show the user every item that is not `pass`, with its `detail` and `refs`.

What is kept and what is not:

- Results and confirmations passed in are recorded in
  `.handoff/release/<milestone>.json` (per command: status, exit code, source,
  time). A call with neither writes nothing.
- A confirmation stays: later calls report that manual item as `pass`.
- A command result does **not** carry over: a call without `results` reports
  the command items as `skip` again and hands the commands back. To get
  `ok: true`, the call that reports the results must be the one you read `ok`
  from (results and `confirm` can go in the same call).
- Define the manual items with
  `handoff_update_config(updates: {"release.manual_checks": ["docs_updated", "store_listing"]})`.

Errors that judge and record nothing: an unknown milestone (the message lists
the defined ones), an unknown argument or result field, a result for something
that is not a command item, a `confirm` id that is not a configured manual
item.

In a terminal the user gets the same check with
`handoff-mcp release-check <milestone>` (`--run` makes the CLI run the commands
itself, with `source: "executed"`; `--confirm manual:<name>` records a
confirmation; exit code 0 = ok, 1 = not ok, 2 = the check could not be carried
out).

## `[commands]` — the project's commands

`[commands]` in `.handoff/config.toml` is the source of truth for the commands
agents and checks use. The `Build & Test` section of `CLAUDE.md` is a copy of
it for people and agents to read.

| Key | Meaning |
|-----|---------|
| `install` / `build` / `test` / `lint` / `format` / `typecheck` | The usual project commands |
| `smoke` | One command that shows the application still starts and works |
| `audit` | Dependency audit |

- Each value is a shell command line (a string); every key is optional.
- `handoff-mcp setup` (run by the user in a terminal) detects commands from
  `package.json` scripts, `Cargo.toml` and `pyproject.toml`, writes the keys
  that have no value yet, and adds the `CLAUDE.md` sections with those values.
  Fields it cannot fill are marked `TODO(handoff)`.
- For any other project layout, write the commands yourself:
  `handoff_update_config(updates: {"commands.test": "make test"})`.
- When a command changes, update `[commands]` first, then the `Build & Test`
  copy in `CLAUDE.md`.
- The MCP server never runs these commands.

## The `beginner` preset

One call sets the configuration for one person building a first application.

- New project: `handoff_init(project_name: "...", preset: "beginner")`.
- Existing project: `handoff_update_config(preset: "beginner")`.

It sets:

| Key | Value |
|-----|-------|
| `trace.profile` | `minimal` |
| `trace.done_guard` | `warn` |
| `session_loop.profile` | `standard` |
| `onboarding.preset` | `beginner` |
| `onboarding.preflight` | `run` |
| `onboarding.next_step` | `true` |
| `onboarding.smoke_check` | `true` |
| `onboarding.release_guard` | `warn` |
| `onboarding.security_min_nfr` | `true` |

On an existing project the reply is JSON
`{preset, applied, changes: [{key, old, new}], overwrites: [key], message}`:

- Keys that had no value are set immediately (`applied: true`).
- When the preset would replace a value the project already set, nothing is
  written (`applied: false`, `overwrites` names the keys). Show `changes` to the
  user and, once they agree, call
  `handoff_update_config(preset: "beginner", confirm: true)`.
- `preset` cannot be combined with `updates` in one call.
- An unknown preset name is an error that lists the accepted names.

## `[onboarding]` keys

All default to off. Set them one by one with
`handoff_update_config(updates: {"onboarding.<key>": value})`; values are
validated on write.

| Key | Values | Default |
|-----|--------|---------|
| `preset` | `beginner` | unset |
| `preflight` | `off` / `check` / `run` | `off` |
| `next_step` | boolean: `handoff_load_context` carries the answer of `handoff_next_step` as its `next_step` key | `false` |
| `smoke_check` | boolean | `false` |
| `release_guard` | `off` / `warn` / `block`: what `handoff_update_milestone(status: "released")` does when an item of the release check is not `pass` (`warn` lists them, `block` rejects the call unless `force: true`) — see below | `off` |
| `security_min_nfr` | boolean | `false` |
| `command_timeout_sec` | seconds (>= 1) | `600` |
| `secret_file_patterns` | list of file-name patterns | `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa`, `id_ed25519`, `credentials.json`, `secrets.*` |

handoff-mcp stores these values; the task-loop commands read them through
`handoff_get_config` to decide which checks to run. `release_guard` is the
exception, with `next_step`: handoff-mcp applies these two itself.

### `release_guard` — the release check at the moment of release

With `release_guard = "warn"` or `"block"`,
`handoff_update_milestone(name, status: "released")` consults the milestone's
release check before it changes the status. It never runs a command and writes
no record:

- `tasks_done`, `no_open_bugs`, `requirements_verified`, `secrets_untracked`
  and `manual:*` are judged on the spot.
- `tests_pass`, `smoke_pass` and `dependencies_audited` are read from the last
  result recorded in `.handoff/release/<milestone>.json`. No record, or a
  record made with a command that `[commands]` no longer names, counts as not
  verified (`skip`).

| Mode | An item is not `pass` |
|------|------------------------|
| `warn` | The milestone is released. The reply ends with `Warning: release check: ...` lines: how many items did not pass, one line per item (`<id> [<status>] <detail>`), and when each recorded command result was judged |
| `block` | The call fails with the same list and nothing is changed. `force: true` releases anyway and returns the list as warnings |

When every item passes, the reply is the plain `Updated milestone '<name>'`.
If the check itself cannot be carried out (for example a broken record file),
`warn` says so in a warning and `block` rejects the release.

A recorded result may be out of date. Before releasing, call
`handoff_release_check`, run the commands and report the `results`; release
only after `ok` is true. Use `force: true` only when the user decided to
release despite the listed items.
