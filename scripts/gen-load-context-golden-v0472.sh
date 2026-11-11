#!/usr/bin/env bash
# Regenerate the v0.47.2 load_context golden fixtures (WS0 of v0.47.3).
#
# Usage: scripts/gen-load-context-golden-v0472.sh [path/to/handoff-mcp-v0.47.2]
#
# Builds two synthetic projects in a temp dir with the REAL v0.47.2 binary's CLI
# (no hand-written .handoff), runs `session load`, normalizes volatile values
# and writes tests/fixtures/load_context_golden_v0472/{small,todo300}.json.
# Due dates are fixed (2020-01-xx = always overdue, 2099-12-31 = never overdue),
# so overdue_count does not depend on the run date.
set -euo pipefail

BIN="${1:-/tmp/golden-v0472/handoff-mcp-v0.47.2}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/tests/fixtures/load_context_golden_v0472"
[ -x "$BIN" ] || { echo "binary not found: $BIN" >&2; exit 1; }
case "$("$BIN" --help 2>&1 | sed -n 1p)" in *v0.47.2*) ;; *) echo "binary is not v0.47.2" >&2; exit 1;; esac

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"

t() { # t <project> <json>
  local out
  out="$("$BIN" task update --project-dir "$1" --task "$2")" || { echo "task update failed: $2" >&2; echo "$out" >&2; exit 1; }
}

# ---- (a) small: parents/children/grandchildren, every status ----
S="$WORK/small"; mkdir -p "$S"
"$BIN" init --project-dir "$S" --project-name golden-small >/dev/null
"$BIN" config update --project-dir "$S" --updates '{"settings.require_estimate_hours":false}' >/dev/null
OLD='"due_date":"2020-01-10"'; FAR='"due_date":"2099-12-31"'
t "$S" '{"id":"t1","title":"Epic A","status":"in_progress","priority":"high","labels":["epic"],"schedule":{'"$FAR"',"milestone":"M1","estimate_hours":20}}'
t "$S" '{"id":"t1.1","title":"A child done","status":"done","priority":"medium","schedule":{'"$OLD"',"milestone":"M1"}}'
t "$S" '{"id":"t1.2","title":"A child in progress","status":"in_progress","priority":"high","assignee":"alice","schedule":{'"$OLD"',"milestone":"M1","estimate_hours":4}}'
t "$S" '{"id":"t1.2.1","title":"A grandchild todo","status":"todo","priority":"low","schedule":{'"$FAR"'}}'
t "$S" '{"id":"t1.2.2","title":"A grandchild review","status":"review","priority":"medium","schedule":{'"$FAR"'}}'
t "$S" '{"id":"t1.3","title":"A child blocked","status":"blocked","priority":"high","dependencies":["t1.1"],"schedule":{'"$OLD"'}}'
t "$S" '{"id":"t2","title":"Epic B","status":"todo","priority":"medium","labels":["feature"],"schedule":{'"$FAR"',"milestone":"M2"}}'
for i in 1 2 3 4 5 6; do
  case $i in 1|2) st=todo;; 3) st=done;; 4) st=skipped;; 5) st=todo;; 6) st=in_progress;; esac
  t "$S" '{"id":"t2.'$i'","title":"B child '$i'","status":"'$st'","priority":"medium","schedule":{'"$FAR"',"milestone":"M2"}}'
done
t "$S" '{"id":"t3","title":"Epic C all done","status":"done","priority":"low"}'
for i in 1 2 3; do t "$S" '{"id":"t3.'$i'","title":"C child '$i'","status":"done","priority":"low"}'; done
for i in $(seq 4 15); do
  case $((i % 3)) in 0) st=todo;; 1) st=todo;; 2) st=done;; esac
  pr=$( [ $((i % 2)) -eq 0 ] && echo high || echo low )
  t "$S" '{"id":"t'$i'","title":"Standalone '$i'","status":"'$st'","priority":"'$pr'","schedule":{'"$( [ $((i % 4)) -eq 0 ] && echo "$OLD" || echo "$FAR" )"'}}'
done
"$BIN" session save --project-dir "$S" --summary "golden previous session" \
  --session-status closed --related-task-ids '["t1.2","t2.1","t2.2"]' >/dev/null

# ---- (b) todo300: 300 flat+nested todos, mixed priority/milestone ----
M="$WORK/todo300"; mkdir -p "$M"
"$BIN" init --project-dir "$M" --project-name golden-todo300 >/dev/null
"$BIN" config update --project-dir "$M" --updates '{"settings.require_estimate_hours":false}' >/dev/null
t "$M" '{"id":"t1","title":"Big parent","status":"in_progress","priority":"high","schedule":{'"$FAR"'}}'
n=1
for i in $(seq 1 240); do
  pr=$(case $((i % 3)) in 0) echo high;; 1) echo medium;; 2) echo low;; esac)
  ms=$( [ $((i % 10)) -eq 0 ] && echo ',"milestone":"M1"' || echo "" )
  due=$( [ $((i % 7)) -eq 0 ] && echo "$OLD" || echo "$FAR" )
  t "$M" '{"id":"t1.'$i'","title":"Big child '$i'","status":"todo","priority":"'$pr'","schedule":{'"$due$ms"'}}'
done
for i in $(seq 2 60); do
  pr=$(case $((i % 3)) in 0) echo high;; 1) echo medium;; 2) echo low;; esac)
  t "$M" '{"id":"t'$i'","title":"Flat todo '$i'","status":"todo","priority":"'$pr'","schedule":{'"$FAR"'}}'
done
"$BIN" session save --project-dir "$M" --summary "golden previous session" \
  --session-status closed --related-task-ids "$(python3 -I -c 'import json;print(json.dumps(["t1.%d"%i for i in range(1,241)]+["t%d"%i for i in range(2,61)]))')" >/dev/null

# ---- load_context + normalize ----
for name in small todo300; do
  "$BIN" session load --project-dir "$WORK/$name" > "$WORK/$name.raw.json"
  python3 -I "$ROOT/scripts/normalize-load-context-golden.py" "$WORK/$name.raw.json" "$OUT/$name.json"
  echo "wrote $OUT/$name.json"
done
