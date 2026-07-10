#!/usr/bin/env bash
#
# sync-workflow-inline.sh — keep workflow scripts self-contained.
#
# The Workflow runtime cannot `import` (probed: "import() is not available in
# workflow scripts"; `require` is undefined), and workflow scripts have a
# top-level `return` so Node cannot import *them* either. Neither side can reach
# the other, yet we still want the shared logic unit-tested.
#
# Resolution: the pure logic lives in testable ES modules (the source of truth).
# Each module's INLINE region is mirrored verbatim into the workflow script
# between GENERATED markers. This script performs — or verifies — that mirror.
#
#   ./scripts/sync-workflow-inline.sh          # rewrite the generated blocks
#   ./scripts/sync-workflow-inline.sh --check  # fail if out of sync (CI/hook)
#
# Mirrors the contract of scripts/sync-plugin-version.sh.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

TARGET="plugin-task-loop/workflows/session-execute.js"

# Modules mirrored into TARGET, in the order they must appear.
# Each entry is a bare module name; source is lib/<name>.js.
MODULES=(verdict-logic profile)

CHECK_MODE=0
if [ "${1:-}" = "--check" ]; then
  CHECK_MODE=1
elif [ $# -gt 0 ]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi

[ -f "$TARGET" ] || { echo "ERROR: missing $TARGET" >&2; exit 1; }

count_marker() { grep -cFx "$1" "$2" || true; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Start from the current target; each module rewrites its own block in turn.
cp "$TARGET" "$WORK/rendered"

for module in "${MODULES[@]}"; do
  SOURCE="plugin-task-loop/workflows/lib/${module}.js"
  [ -f "$SOURCE" ] || { echo "ERROR: missing $SOURCE" >&2; exit 1; }

  SRC_BEGIN="// --- BEGIN INLINE: ${module} ---"
  SRC_END="// --- END INLINE: ${module} ---"
  DST_BEGIN="// --- BEGIN GENERATED: ${module} (source: lib/${module}.js) ---"
  DST_END="// --- END GENERATED: ${module} ---"

  for marker in "$SRC_BEGIN" "$SRC_END"; do
    n=$(count_marker "$marker" "$SOURCE")
    [ "$n" -eq 1 ] || { echo "ERROR: '$marker' appears $n time(s) in $SOURCE (want 1)" >&2; exit 1; }
  done
  for marker in "$DST_BEGIN" "$DST_END"; do
    n=$(count_marker "$marker" "$WORK/rendered")
    [ "$n" -eq 1 ] || { echo "ERROR: '$marker' appears $n time(s) in $TARGET (want 1)" >&2; exit 1; }
  done

  EXTRACTED="$WORK/${module}.body"
  awk -v b="$SRC_BEGIN" -v e="$SRC_END" '
    $0 == b { inblk = 1; next }
    $0 == e { inblk = 0; next }
    inblk   { print }
  ' "$SOURCE" > "$EXTRACTED"

  if [ ! -s "$EXTRACTED" ]; then
    echo "ERROR: extracted inline region from $SOURCE is empty" >&2
    exit 1
  fi

  # The mirrored region must be self-contained.
  if grep -Eq '^[[:space:]]*(import|export)[[:space:]]' "$EXTRACTED"; then
    echo "ERROR: the inline region of $SOURCE contains import/export." >&2
    echo "       Workflow scripts cannot import. Keep those outside the markers." >&2
    exit 1
  fi

  # A destination marker inside the copied body would corrupt the target — the
  # awk pass would end the generated block early and duplicate the remainder,
  # after which every --check fails with a confusing "appears 2 time(s)".
  for marker in "$DST_BEGIN" "$DST_END"; do
    if grep -qFx "$marker" "$EXTRACTED"; then
      echo "ERROR: the inline region of $SOURCE contains the destination marker:" >&2
      echo "       $marker" >&2
      echo "       Remove it — it would corrupt $TARGET on the next sync." >&2
      exit 1
    fi
  done

  awk -v b="$DST_BEGIN" -v e="$DST_END" -v body="$EXTRACTED" -v src="$SOURCE" '
    $0 == b {
      print
      print "// AUTO-GENERATED — DO NOT EDIT BY HAND."
      print "// Source: " src
      print "// Regenerate: ./scripts/sync-workflow-inline.sh"
      while ((getline line < body) > 0) print line
      close(body)
      skip = 1
      next
    }
    $0 == e { skip = 0; print; next }
    !skip   { print }
  ' "$WORK/rendered" > "$WORK/next"
  mv "$WORK/next" "$WORK/rendered"
done

if [ "$CHECK_MODE" -eq 1 ]; then
  if ! diff -u "$TARGET" "$WORK/rendered" > /dev/null 2>&1; then
    echo "ERROR: $TARGET is out of sync with its source modules" >&2
    echo "       Run ./scripts/sync-workflow-inline.sh to regenerate." >&2
    echo >&2
    diff -u "$TARGET" "$WORK/rendered" >&2 || true
    exit 1
  fi
  echo "OK: $TARGET is in sync with ${MODULES[*]}"
  exit 0
fi

if diff -q "$TARGET" "$WORK/rendered" > /dev/null 2>&1; then
  echo "OK: $TARGET already in sync (no change)"
else
  cat "$WORK/rendered" > "$TARGET"
  echo "Synced: ${MODULES[*]} -> $TARGET"
fi
