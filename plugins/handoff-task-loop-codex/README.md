# Handoff Task Loop for Codex

`$handoff-session-loop` is an explicitly started, Handoff-backed task loop for
Codex. It selects ready tasks, delegates only independent work to Codex child
agents, and keeps task state and the session handoff current.

It is a Codex-native plugin. It does not execute or modify the Claude Code
`/session-loop`, `/research-loop`, `commands/`, `agents/`, or `workflows/`
assets in this repository.

The Handoff MCP server must be available in the project before starting a
loop.

## Installation

### Recommended: GitHub marketplace

Install the Handoff binary, register the marketplace, install the task-loop
plugin, and register Handoff as a Codex MCP server:

```bash
npm install -g handoff-mcp-server
codex plugin marketplace add alphaelements/handoff-mcp
codex plugin add handoff-task-loop-codex@handoff-mcp-marketplace
codex mcp add handoff -- handoff-mcp
```

Verify the installation, then start a new Codex session so it discovers the
installed skill:

```bash
handoff-mcp --version
codex plugin list
codex mcp get handoff
```

In the new session, explicitly invoke `$handoff-session-loop`. The loop does
not start automatically.

For the normal interactive CLI, Codex asks for Handoff writes as required. For
non-interactive `codex exec`, opt in only for a controlled run by allowing the
Handoff server's tool writes:

```bash
codex exec -c 'mcp_servers.handoff.default_tools_approval_mode = "auto"' \
  --sandbox workspace-write '$handoff-session-loop <scoped task request>'
```

Without that setting, Codex can cancel Handoff write calls; do not use
non-interactive mode for a task loop until the setting is deliberate.

### Local development checkout

From a checkout of this repository, replace the GitHub source with its absolute
path:

```bash
codex plugin marketplace add /absolute/path/to/handoff-mcp
codex plugin add handoff-task-loop-codex@handoff-mcp-marketplace
codex mcp add handoff -- /absolute/path/to/handoff-mcp/target/release/handoff-mcp
```

Use a stable binary path or install the npm package instead of relying on a
debug build. After changing the plugin, reinstall it and begin a new Codex
session; an already-running session does not reload its skills.

### Direct MCP only

If you do not want the task-loop plugin, install the binary and register only
the MCP server:

```bash
npm install -g handoff-mcp-server
codex mcp add handoff -- handoff-mcp
```

This provides Handoff tools but not `$handoff-session-loop`. Project-specific
guidance can still be placed in `AGENTS.md`.

## Updating and troubleshooting

For a Git marketplace, refresh the marketplace before reinstalling the plugin:

```bash
codex plugin marketplace upgrade handoff-mcp-marketplace
codex plugin add handoff-task-loop-codex@handoff-mcp-marketplace
npm install -g handoff-mcp-server@latest
```

- If the skill does not appear, start a new Codex session and check
  `codex plugin list` for `installed, enabled`.
- If Handoff tools are unavailable, run `codex mcp get handoff` and confirm
  `handoff-mcp` is on `PATH`.
- If a local marketplace is stale, re-add its absolute path and reinstall the
  plugin. Do not edit Codex's plugin cache directly.

## Claude Code compatibility

This plugin is intentionally separate from the Claude Code task loop.
Claude users invoke `/session-loop` or `/research-loop`; Codex users invoke
`$handoff-session-loop`. Codex does not execute the Claude `commands/`,
`agents/`, or `workflows/` DSL, and the two plugin implementations must remain
independent.
