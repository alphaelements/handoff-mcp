#!/usr/bin/env bash
set -euo pipefail

# Verify that the lockstep `typst*` crates resolve to a single version
# (REQ-PDF-041, wiki/390-typst-library-bundling.md §6 Layer 2).
#
# The compiler crates (typst, typst-pdf, typst-library, typst-syntax, ...) are
# released together; a partial upgrade (e.g. Dependabot bumping one crate but
# not another) leaves two compiler versions in the tree and breaks the build or
# silently doubles the binary. The typst-as-lib wrapper has its own release
# cadence (0.17.x against typst 0.15.x) and is excluded.
#
# Zero typst crates in the tree passes: nothing to align.
#
# Usage: ./scripts/check-typst-versions.sh [metadata.json]
#   metadata.json: a saved `cargo metadata` document to judge instead of the
#   live workspace (used by the self-test in scripts/check-typst-versions.test.sh).

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if ! command -v jq >/dev/null 2>&1; then
  echo "ERROR: jq is required for the typst version check" >&2
  exit 1
fi

# --locked: judge the committed Cargo.lock, never re-resolve it as a side effect.
# --all-features: without it cargo metadata omits the optional `bundled-typst`
# dependencies, so the check would pass vacuously on a default build.
if [ -n "${1:-}" ]; then
  metadata="$(cat "$1")"
else
  metadata="$(cargo metadata --format-version=1 --locked --all-features --manifest-path "$ROOT/Cargo.toml")"
fi
versions="$(printf '%s' "$metadata" \
  | jq -r '[.packages[]
            | select(.name | startswith("typst"))
            | select(.name | . == "typst-as-lib" | not)
            | .version] | unique | join(" ")')"

count="$(printf '%s' "$versions" | wc -w | tr -d ' ')"
if [ "$count" -gt 1 ]; then
  echo "ERROR: typst crate versions are not aligned: $versions" >&2
  echo "All typst* compiler crates must resolve to the same version (wiki/390 §6)." >&2
  exit 1
fi
echo "typst crate versions aligned (${versions:-none in tree})."
