#!/usr/bin/env bash
# Codex CLI UserPromptSubmit hook — inject relevant handoff project memories.
#
# Codex hooks only support `type: "command"` (prompt/agent/async handlers are
# stubbed out in 0.145.x), so this shells out to the `handoff-mcp` CLI rather
# than calling the MCP tool directly the way the Claude Code hooks do.
#
# Contract:
#   stdin  — Codex hook payload, JSON with at least `cwd` and `prompt`
#   stdout — JSON with hookSpecificOutput.additionalContext (injected verbatim)
#
# Must always exit 0 and emit valid JSON: a non-zero exit or malformed stdout
# turns a missing memory into a broken turn. No memories => empty output, which
# Codex treats as "nothing to inject".

set -uo pipefail

HANDOFF_BIN="${HANDOFF_MCP_BIN:-handoff-mcp}"
LIMIT="${HANDOFF_MEMORY_LIMIT:-5}"

emit_nothing() { printf '%s' '{}'; exit 0; }

payload="$(cat)"
[ -n "$payload" ] || emit_nothing

command -v "$HANDOFF_BIN" >/dev/null 2>&1 || emit_nothing

# Prefer jq, but stay functional without it — hooks run on whatever the user has.
if command -v jq >/dev/null 2>&1; then
  prompt="$(printf '%s' "$payload" | jq -r '.prompt // empty' 2>/dev/null)"
  cwd="$(printf '%s' "$payload" | jq -r '.cwd // empty' 2>/dev/null)"
else
  # Non-greedy by construction: `[^"]*` stops at the closing quote so trailing
  # keys are not swallowed into the prompt. An escaped quote inside the prompt
  # truncates it, which only costs some query terms — the jq path above is exact.
  prompt="$(printf '%s' "$payload" | sed -n 's/.*"prompt"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
  cwd="$(printf '%s' "$payload" | sed -n 's/.*"cwd"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
fi

[ -n "$prompt" ] || emit_nothing
[ -n "$cwd" ] || cwd="$PWD"

# A project without .handoff/ is not an error — it just has nothing to inject.
[ -d "$cwd/.handoff" ] || emit_nothing

# Belt-and-braces timeout. Codex enforces `timeoutSec` on the hook, but a hung
# query would otherwise stall every turn up to that limit; cap it ourselves so
# the turn keeps moving even if the store is wedged.
if command -v timeout >/dev/null 2>&1; then
  result="$(timeout "${HANDOFF_MEMORY_TIMEOUT:-10}" "$HANDOFF_BIN" memory query \
    --project-dir "$cwd" \
    --text -- "$prompt" \
    --limit "$LIMIT" 2>/dev/null)" || emit_nothing
else
  result="$("$HANDOFF_BIN" memory query \
    --project-dir "$cwd" \
    --text -- "$prompt" \
    --limit "$LIMIT" 2>/dev/null)" || emit_nothing
fi

[ -n "$result" ] || emit_nothing

if command -v jq >/dev/null 2>&1; then
  printf '%s' "$result" | jq -c '
    if (.memories // []) | length == 0 then
      {}
    else
      {
        hookSpecificOutput: {
          hookEventName: "UserPromptSubmit",
          additionalContext: (
            "## Project memory (handoff-mcp)\n\n"
            + "Relevant durable knowledge for this project. Treat as established context.\n\n"
            + ([.memories[] | "- (\(.kind // "note")) \(.text)"] | join("\n\n"))
          )
        }
      }
    end
  ' 2>/dev/null || emit_nothing
else
  # Without jq, inject the raw tool JSON rather than dropping the memories.
  python3 - "$result" <<'PY' 2>/dev/null || emit_nothing
import json, sys
try:
    data = json.loads(sys.argv[1])
    mems = data.get("memories") or []
    if not mems:
        print("{}"); sys.exit(0)
    body = "\n\n".join(f"- ({m.get('kind') or 'note'}) {m.get('text','')}" for m in mems)
    ctx = ("## Project memory (handoff-mcp)\n\n"
           "Relevant durable knowledge for this project. Treat as established context.\n\n" + body)
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "UserPromptSubmit", "additionalContext": ctx}}))
except Exception:
    print("{}")
PY
fi
