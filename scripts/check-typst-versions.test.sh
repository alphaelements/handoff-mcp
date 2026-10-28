#!/usr/bin/env bash
set -euo pipefail

# Self-test for check-typst-versions.sh: aligned, misaligned and typst-free
# dependency sets, using synthetic `cargo metadata` documents.

HERE="$(cd "$(dirname "$0")" && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

meta() { # name@version ...
  local out='{"packages":['
  local first=1
  for nv in "$@"; do
    [ "$first" -eq 1 ] || out+=','
    first=0
    out+="{\"name\":\"${nv%@*}\",\"version\":\"${nv#*@}\"}"
  done
  printf '%s]}' "$out"
}

fail=0
expect() { # expected-exit description metadata-args...
  local want="$1" desc="$2"
  shift 2
  meta "$@" >"$TMP/m.json"
  local got=0
  "$HERE/check-typst-versions.sh" "$TMP/m.json" >/dev/null 2>&1 || got=$?
  if [ "$got" -ne "$want" ]; then
    echo "FAIL: $desc (exit $got, wanted $want)" >&2
    fail=1
  else
    echo "ok: $desc"
  fi
}

expect 0 "aligned compiler crates, wrapper on its own version" \
  typst@0.15.1 typst-pdf@0.15.1 typst-library@0.15.1 typst-as-lib@0.17.0 serde@1.0.0
expect 1 "partially upgraded compiler crates" \
  typst@0.15.1 typst-pdf@0.16.0 typst-as-lib@0.17.0
expect 0 "no typst crates in the tree" serde@1.0.0 anyhow@1.0.0
exit "$fail"
