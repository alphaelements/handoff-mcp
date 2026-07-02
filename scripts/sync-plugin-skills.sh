#!/usr/bin/env bash
# Sync skills/ -> plugin/skills/ for plugin distribution.
# Run before release to ensure the plugin has the latest skills.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"

rm -rf "$ROOT/plugin/skills"
cp -a "$ROOT/skills" "$ROOT/plugin/skills"

echo "Synced skills/ -> plugin/skills/"
diff -rq "$ROOT/skills/" "$ROOT/plugin/skills/" && echo "No differences." || echo "WARNING: differences remain after sync."
