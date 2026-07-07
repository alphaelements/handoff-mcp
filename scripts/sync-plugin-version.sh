#!/usr/bin/env bash
# Sync plugin/marketplace manifest versions to package.json's version.
#
# package.json is the source of truth (kept in sync with Cargo.toml manually
# per CLAUDE.md "Version sync" rule). This script propagates that version to:
#   - plugin/.claude-plugin/plugin.json
#   - plugin-hooks/.claude-plugin/plugin.json
#   - plugin-task-loop/.claude-plugin/plugin.json
#   - .claude-plugin/marketplace.json (all three "version" entries)
#
# Uses regex line replacement (not json.dump) so untouched formatting in
# these hand-maintained manifests is preserved byte-for-byte.
#
# Usage:
#   ./scripts/sync-plugin-version.sh          # fix mismatches in place
#   ./scripts/sync-plugin-version.sh --check  # exit 1 if anything is out of sync, fix nothing
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"
cd "$ROOT"

MODE="fix"
if [ "${1:-}" = "--check" ]; then
  MODE="check"
fi

python3 - "$MODE" <<'PYEOF'
import json
import re
import sys

mode = sys.argv[1]

with open("package.json") as f:
    root_version = json.load(f)["version"]

version_line_re = re.compile(r'^(\s*"version"\s*:\s*)"[^"]*"(,?\s*(?:\r?\n)?)$')

def sync_file(path, mismatches):
    with open(path) as f:
        lines = f.readlines()
    changed = False
    out = []
    for line in lines:
        m = version_line_re.match(line)
        if m:
            current = re.search(r'"version"\s*:\s*"([^"]*)"', line).group(1)
            if current != root_version:
                mismatches.append((path, current))
                changed = True
                line = f'{m.group(1)}"{root_version}"{m.group(2)}'
        out.append(line)
    if changed and mode == "fix":
        with open(path, "w") as f:
            f.writelines(out)
    return changed

mismatches = []
plugin_files = [
    "plugin/.claude-plugin/plugin.json",
    "plugin-hooks/.claude-plugin/plugin.json",
    "plugin-task-loop/.claude-plugin/plugin.json",
    ".claude-plugin/marketplace.json",
]

for path in plugin_files:
    sync_file(path, mismatches)

if mode == "check":
    if mismatches:
        print(f"Version mismatch: package.json is {root_version}, but found:")
        for path, version in mismatches:
            print(f"  {path}: {version}")
        print("Run ./scripts/sync-plugin-version.sh to fix.")
        sys.exit(1)
    print(f"All plugin/marketplace manifests match package.json ({root_version}).")
else:
    if mismatches:
        print(f"Synced {len(mismatches)} version occurrence(s) to {root_version}:")
        for path, old_version in mismatches:
            print(f"  {path}: {old_version} -> {root_version}")
    else:
        print(f"Already in sync at version {root_version}. No changes made.")
PYEOF
