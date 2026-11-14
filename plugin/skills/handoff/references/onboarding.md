# Readiness check, project commands, the beginner preset and `[onboarding]`

How to tell whether a project is ready for agent-driven work (`handoff_doctor`),
and the settings that get it ready and switch on guidance for first-time users.
The settings are optional and off by default: a project that sets none of them
behaves as before.

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

`status` is `pass`, `fail`, `skip` or `manual`. `skip` means the check was not
carried out (no result was given, or nothing is defined); it is never a pass.
Every `fail` carries a `next_step` that says what to do. `ready` is true when
nothing failed; a `skip` does not lower it.

**The tool never runs a command.** Running them is your job:

1. Call `handoff_doctor()`. Every `cmd_*` check is `skip`, and
   `commands_to_run` lists the commands that are defined.
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

`checks: ["cmd_test", ...]` limits the diagnosis to the named checks. `format`,
`smoke` and `audit` are never part of `commands_to_run`.

A call the tool cannot take at its word is an error and judges nothing: an
argument or result field it does not know (a misspelled `results` is not
silently dropped), an unknown or empty `checks`, a result for something that is
not a `cmd_*` check or not among the requested `checks`, two results for one
check, a result with neither `exit_code` nor `error`, or an empty `error`.

In a terminal the user gets the same diagnosis with `handoff-mcp setup --check`
(`--run` makes the CLI run the commands itself, with `source: "executed"`;
`--json` prints the JSON above; the exit code is 1 when the project is not
ready).

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
| `next_step` | boolean | `false` |
| `smoke_check` | boolean | `false` |
| `release_guard` | `off` / `warn` / `block` | `off` |
| `security_min_nfr` | boolean | `false` |
| `command_timeout_sec` | seconds (>= 1) | `600` |

handoff-mcp stores these values; the task-loop commands read them through
`handoff_get_config` to decide which checks to run.
