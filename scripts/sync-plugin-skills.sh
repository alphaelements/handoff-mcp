#!/usr/bin/env bash
# Sync skills/ -> plugin/skills/ for plugin distribution.
#
# plugin/skills/ is generated, but it is COMMITTED: the marketplace serves the
# plugin straight from the repository, so a skill that is not committed there
# never reaches an installed plugin. Run this after editing skills/, and commit
# the result.
#
#   sync-plugin-skills.sh            sync skills/ -> plugin/skills/
#   sync-plugin-skills.sh --check    exit non-zero if they differ (no writes)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"

if [ "${1:-}" = "--check" ]; then
  if [ ! -d "$ROOT/plugin/skills" ]; then
    echo "ERROR: plugin/skills/ is missing. Run './scripts/sync-plugin-skills.sh'." >&2
    exit 1
  fi
  if ! diff -rq "$ROOT/skills/" "$ROOT/plugin/skills/" >/dev/null 2>&1; then
    echo "ERROR: plugin/skills/ is out of sync with skills/:" >&2
    diff -rq "$ROOT/skills/" "$ROOT/plugin/skills/" >&2 || true
    echo "Run './scripts/sync-plugin-skills.sh' and commit the result." >&2
    exit 1
  fi
  echo "OK: plugin/skills/ is in sync with skills/."
  exit 0
fi

rm -rf "$ROOT/plugin/skills"
cp -a "$ROOT/skills" "$ROOT/plugin/skills"

echo "Synced skills/ -> plugin/skills/"
diff -rq "$ROOT/skills/" "$ROOT/plugin/skills/" && echo "No differences." || echo "WARNING: differences remain after sync."
