#!/usr/bin/env python3
"""Claude Code hook wrapper for handoff-mcp project memory.

This is the **fallback path** for memory auto-injection. The preferred wiring is
a native ``mcp_tool`` hook that calls ``handoff_memory_query`` / ``handoff_memory_cleanup``
directly (see README "Project Memory"); this script exists for Claude Code
versions that lack the ``mcp_tool`` hook type, by translating a Claude Code hook
event into a single JSON-RPC ``tools/call`` against the handoff-mcp server and
emitting the result as ``hookSpecificOutput.additionalContext``.

It speaks the server's line-delimited JSON-RPC over stdio: one request object on
one line in, one response object on one line out. ``handoff_memory_query`` /
``handoff_memory_cleanup`` return their payload as a JSON *string* inside
``result.content[0].text`` (so both this wrapper and the native hook path parse
it identically), which this script re-parses.

Wiring (in ``~/.claude/settings.json`` — do NOT commit this into a repo):

    {
      "hooks": {
        "UserPromptSubmit": [
          { "hooks": [ { "type": "command",
              "command": "handoff-mcp-memory-hook" } ] }
        ],
        "PreToolUse": [
          { "matcher": "Edit|Write|MultiEdit",
            "hooks": [ { "type": "command",
              "command": "handoff-mcp-memory-hook" } ] }
        ],
        "SessionStart": [
          { "hooks": [ { "type": "command",
              "command": "handoff-mcp-memory-hook" } ] }
        ]
      }
    }

The same script handles all three events; it picks the tool from
``hook_event_name`` on stdin.

Environment:
- ``HANDOFF_MCP_BIN`` — path to the ``handoff-mcp`` binary (default: ``handoff-mcp``
  resolved on ``PATH``).

Design contract: this script must **never break the session**. On any error
(binary missing, bad JSON, timeout) it prints nothing and exits 0 — a memory
miss is silent, never a blocked prompt.
"""

# `X | None` annotations are evaluated lazily so this runs on Python 3.7+
# (a hook host may not have 3.10). The annotations are never evaluated at runtime.
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys

# Map a Claude Code hook event to the memory tool it drives.
QUERY_EVENTS = {"UserPromptSubmit", "PreToolUse"}
CLEANUP_EVENTS = {"SessionStart"}

# How long to wait for the server to answer one request, in seconds. The query
# is in-memory and sub-millisecond; this is just a safety net so a hung binary
# can never stall the prompt.
TIMEOUT_SECONDS = 5.0


def _server_bin() -> str | None:
    """Resolve the handoff-mcp binary path, or None if it can't be found."""
    explicit = os.environ.get("HANDOFF_MCP_BIN")
    if explicit:
        return explicit if os.path.exists(explicit) else None
    return shutil.which("handoff-mcp")


def _call(bin_path: str, tool: str, arguments: dict) -> dict | None:
    """Send one ``tools/call`` line and parse the inner JSON payload.

    Returns the decoded inner object (e.g. ``{"memories": [...]}`), or None on
    any transport / decode failure. The server is stateless per line, so no
    ``initialize`` handshake is needed — a bare ``tools/call`` is answered.
    """
    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments},
    }
    try:
        proc = subprocess.run(
            [bin_path],
            input=json.dumps(request) + "\n",
            capture_output=True,
            text=True,
            timeout=TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None

    # The response is the first non-empty stdout line (stderr carries the
    # startup banner and is ignored).
    line = next((ln for ln in proc.stdout.splitlines() if ln.strip()), None)
    if not line:
        return None
    try:
        envelope = json.loads(line)
        text = envelope["result"]["content"][0]["text"]
        return json.loads(text)
    except (json.JSONDecodeError, KeyError, IndexError, TypeError):
        return None


def _file_paths_from_tool_input(tool_input) -> list[str]:
    """Pull file paths out of a PreToolUse ``tool_input`` (Edit/Write/MultiEdit)."""
    if not isinstance(tool_input, dict):
        return []
    paths = []
    fp = tool_input.get("file_path")
    if isinstance(fp, str) and fp:
        paths.append(fp)
    # MultiEdit and similar may carry an edits array; each can name a file.
    for edit in tool_input.get("edits", []) or []:
        if isinstance(edit, dict):
            efp = edit.get("file_path")
            if isinstance(efp, str) and efp:
                paths.append(efp)
    return paths


def _emit(event: str, context: str) -> None:
    """Print a Claude Code hook output object carrying additionalContext."""
    if not context:
        return
    out = {
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    }
    print(json.dumps(out))


def _format_memories(payload: dict) -> str:
    """Turn a ``handoff_memory_query`` payload into an injectable context block."""
    memories = payload.get("memories") or []
    if not memories:
        return ""
    lines = ["Relevant project memories (handoff-mcp):"]
    for m in memories:
        text = (m.get("text") or "").strip()
        if not text:
            continue
        kind = m.get("kind") or "memory"
        lines.append(f"- [{kind}] {text}")
    # Only a header with no bullets means nothing useful to inject.
    return "\n".join(lines) if len(lines) > 1 else ""


def main() -> int:
    raw = sys.stdin.read()
    try:
        hook = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError:
        return 0  # malformed hook input — stay silent, never block

    event = hook.get("hook_event_name") or ""
    bin_path = _server_bin()
    if not bin_path:
        return 0  # server not installed — nothing to inject

    project_dir = hook.get("cwd") or os.getcwd()
    session_id = hook.get("session_id")

    if event in QUERY_EVENTS:
        # UserPromptSubmit → match the prompt. PreToolUse → match the file(s).
        prompt = hook.get("prompt") or ""
        tool_name = hook.get("tool_name")
        file_paths = _file_paths_from_tool_input(hook.get("tool_input"))
        # Text fed to BM25: the prompt for UserPromptSubmit, else the file paths
        # (the server also adds basenames + tool_name to the query).
        text = prompt if prompt else " ".join(file_paths)
        arguments = {"project_dir": project_dir, "text": text}
        if session_id:
            arguments["session_id"] = session_id
        if tool_name:
            arguments["tool_name"] = tool_name
        if file_paths:
            arguments["file_paths"] = file_paths

        payload = _call(bin_path, "handoff_memory_query", arguments)
        if payload:
            _emit(event, _format_memories(payload))
        return 0

    if event in CLEANUP_EVENTS:
        # Housekeeping only: merge exact duplicates and gc sidecars. We do not
        # inject the cleanup recommendations as context (that is for an explicit
        # AI-driven pass), so there is no additionalContext to emit here.
        _call(bin_path, "handoff_memory_cleanup", {"project_dir": project_dir})
        return 0

    # Unknown event — nothing to do.
    return 0


if __name__ == "__main__":
    sys.exit(main())
