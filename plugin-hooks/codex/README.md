# Codex CLI — handoff memory auto-injection hook

Injects relevant handoff project memories into every Codex turn, the same way
the Claude Code `handoff-mcp-hooks` plugin does.

Verified against **Codex CLI 0.145.0**.

## Why a shell script instead of an MCP tool call

The Claude Code hooks in `../hooks/hooks.json` use `"type": "mcp_tool"` to call
`handoff_memory_query` directly. Codex does not support that:

- Codex hook handler types are `command`, `prompt`, and `agent`, but the
  binary reports *"prompt hooks are not supported yet"* and *"agent hooks are
  not supported yet"* — **only `command` actually runs**.
- There is no `mcp_tool` handler type at all.

So the hook shells out to the `handoff-mcp` CLI (`handoff-mcp memory query`),
which returns the same JSON the MCP tool does.

## Install

1. Install the `handoff-mcp` binary and make sure it is on `PATH`:

   ```bash
   handoff-mcp --version
   ```

2. Copy the hook script somewhere stable and make it executable:

   ```bash
   install -m 755 handoff-memory-inject.sh ~/.codex/handoff-memory-inject.sh
   ```

3. Merge `hooks.json` into `~/.codex/hooks.json`, replacing
   `/ABSOLUTE/PATH/TO/handoff-memory-inject.sh` with the real path:

   ```json
   {
     "hooks": {
       "UserPromptSubmit": [
         {
           "hooks": [
             {
               "type": "command",
               "command": "/home/you/.codex/handoff-memory-inject.sh",
               "timeoutSec": 15,
               "statusMessage": "handoff: injecting project memory"
             }
           ]
         }
       ]
     }
   }
   ```

   The path **must be absolute**. Codex has no plugin-root placeholder —
   `${CODEX_PLUGIN_ROOT}` does not expand and makes the hook fail.

4. **Trust the hook.** This is the step that silently breaks everything else.
   Codex will not run an untrusted hook, and it does *not* warn you — the turn
   just proceeds with nothing injected. Trust it interactively via `/hooks` in
   the Codex TUI, or bypass trust per-invocation for non-interactive runs:

   ```bash
   codex exec --dangerously-bypass-hook-trust "..."
   ```

   Trust is keyed to a hash of the exact hook definition, so **editing a
   trusted hook re-blocks it** until you re-approve.

## Verify it works

```bash
printf '{"cwd":"'"$PWD"'","prompt":"test"}' | ~/.codex/handoff-memory-inject.sh
```

A project with memories prints `hookSpecificOutput.additionalContext`; a
project without `.handoff/` prints `{}`.

Inside a real session, Codex prints `hook: UserPromptSubmit Completed` when
the hook ran. `Failed` means a bad path or a non-zero exit; no line at all
means the hook was skipped as untrusted.

## Configuration

| Env var | Default | Meaning |
|---|---|---|
| `HANDOFF_MCP_BIN` | `handoff-mcp` | Path to the binary |
| `HANDOFF_MEMORY_LIMIT` | `5` | Max memories injected per turn |
| `HANDOFF_MEMORY_TIMEOUT` | `10` | Seconds before the query is abandoned |

## Fail-safe behavior

The hook always exits `0` and always prints valid JSON. If the binary is
missing, the project has no `.handoff/`, stdin is empty or malformed, or the
query fails or hangs, it prints `{}` and the turn proceeds normally.

`jq` handles both parsing the payload and building the output. Without it the
hook still works, falling back to `sed` for parsing and `python3` for output —
the `sed` path is lossier (a prompt containing an escaped quote is truncated at
that quote), so installing `jq` is recommended but not required. With neither
`jq` nor `python3` present, the hook injects nothing rather than failing.

## Known constraints

- `codex exec` cancels **MCP tool calls** by default
  ([#24135](https://github.com/openai/codex/issues/24135),
  [#29857](https://github.com/openai/codex/issues/29857)); set
  `default_tools_approval_mode = "auto"` under `[mcp_servers.handoff]`.
  This hook is unaffected — it uses the CLI, not MCP.
- Project-level `<project>/.codex/hooks.json` did not fire in testing; only
  user-level `~/.codex/hooks.json` is verified working.
- Only the `UserPromptSubmit` event is wired up. Codex does also deliver
  `PreToolUse` with `tool_name` and `tool_input` (verified: a `Bash` call
  arrives as `{"tool_name":"Bash","tool_input":{"command":"echo hello"}}`), so
  a file-edit-triggered injection like the Claude Code `PreToolUse` hook is
  feasible — it just is not implemented yet.
