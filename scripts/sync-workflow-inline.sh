#!/usr/bin/env bash
#
# sync-workflow-inline.sh — keep workflow scripts self-contained.
#
# The Workflow runtime cannot `import` (probed: "import() is not available in
# workflow scripts"; `require` is undefined), and workflow scripts have a
# top-level `return` so Node cannot import *them* either. Neither side can reach
# the other, yet we still want the verdict logic unit-tested.
#
# Resolution: the pure logic lives in a testable ES module (the source of
# truth). Its INLINE region is mirrored verbatim into the workflow script
# between GENERATED markers. This script performs — or verifies — that mirror.
#
#   ./scripts/sync-workflow-inline.sh          # rewrite the generated block
#   ./scripts/sync-workflow-inline.sh --check  # fail if out of sync (CI/hook)
#
# Mirrors the contract of scripts/sync-plugin-version.sh.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

SOURCE="plugin-task-loop/workflows/lib/verdict-logic.js"
TARGET="plugin-task-loop/workflows/session-execute.js"

# Markers. The source delimits the region to copy; the target delimits where it lands.
SRC_BEGIN="// --- BEGIN INLINE: verdict-logic ---"
SRC_END="// --- END INLINE: verdict-logic ---"
DST_BEGIN="// --- BEGIN GENERATED: verdict-logic (source: lib/verdict-logic.js) ---"
DST_END="// --- END GENERATED: verdict-logic ---"

CHECK_MODE=0
if [ "${1:-}" = "--check" ]; then
  CHECK_MODE=1
elif [ $# -gt 0 ]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi

for f in "$SOURCE" "$TARGET"; do
  [ -f "$f" ] || { echo "ERROR: missing $f" >&2; exit 1; }
done

# --- Extract the inline region from the source of truth -------------------
# Strict: both markers must exist exactly once.
count_marker() { grep -cFx "$1" "$2" || true; }

for marker in "$SRC_BEGIN" "$SRC_END"; do
  n=$(count_marker "$marker" "$SOURCE")
  [ "$n" -eq 1 ] || { echo "ERROR: '$marker' appears $n time(s) in $SOURCE (want 1)" >&2; exit 1; }
done
for marker in "$DST_BEGIN" "$DST_END"; do
  n=$(count_marker "$marker" "$TARGET")
  [ "$n" -eq 1 ] || { echo "ERROR: '$marker' appears $n time(s) in $TARGET (want 1)" >&2; exit 1; }
done

EXTRACTED="$(mktemp)"
RENDERED="$(mktemp)"
trap 'rm -f "$EXTRACTED" "$RENDERED"' EXIT

# Body strictly between the source markers (markers themselves excluded).
awk -v b="$SRC_BEGIN" -v e="$SRC_END" '
  $0 == b { inblk = 1; next }
  $0 == e { inblk = 0; next }
  inblk   { print }
' "$SOURCE" > "$EXTRACTED"

if [ ! -s "$EXTRACTED" ]; then
  echo "ERROR: extracted inline region from $SOURCE is empty" >&2
  exit 1
fi

# Guard: the mirrored region must be self-contained.
if grep -Eq '^[[:space:]]*(import|export)[[:space:]]' "$EXTRACTED"; then
  echo "ERROR: the inline region of $SOURCE contains import/export." >&2
  echo "       Workflow scripts cannot import. Keep those outside the markers." >&2
  exit 1
fi

# Guard: a destination marker inside the copied body would corrupt the target —
# the awk pass would terminate the generated block early, duplicating the rest,
# and every later --check would fail with a confusing "appears 2 time(s)".
for marker in "$DST_BEGIN" "$DST_END"; do
  if grep -qFx "$marker" "$EXTRACTED"; then
    echo "ERROR: the inline region of $SOURCE contains the destination marker:" >&2
    echo "       $marker" >&2
    echo "       Remove it — it would corrupt $TARGET on the next sync." >&2
    exit 1
  fi
done

# --- Render the target with the generated block replaced ------------------
awk -v b="$DST_BEGIN" -v e="$DST_END" -v body="$EXTRACTED" '
  $0 == b {
    print
    print "// AUTO-GENERATED — DO NOT EDIT BY HAND."
    print "// Source: plugin-task-loop/workflows/lib/verdict-logic.js"
    print "// Regenerate: ./scripts/sync-workflow-inline.sh"
    while ((getline line < body) > 0) print line
    close(body)
    skip = 1
    next
  }
  $0 == e { skip = 0; print; next }
  !skip   { print }
' "$TARGET" > "$RENDERED"

if [ "$CHECK_MODE" -eq 1 ]; then
  if ! diff -u "$TARGET" "$RENDERED" > /dev/null 2>&1; then
    echo "ERROR: $TARGET is out of sync with $SOURCE" >&2
    echo "       Run ./scripts/sync-workflow-inline.sh to regenerate." >&2
    echo >&2
    diff -u "$TARGET" "$RENDERED" >&2 || true
    exit 1
  fi
  echo "OK: $TARGET is in sync with $SOURCE"
  exit 0
fi

if diff -q "$TARGET" "$RENDERED" > /dev/null 2>&1; then
  echo "OK: $TARGET already in sync (no change)"
else
  cat "$RENDERED" > "$TARGET"
  echo "Synced: $SOURCE -> $TARGET (generated block updated)"
fi
