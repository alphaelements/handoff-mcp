#!/usr/bin/env bash
set -euo pipefail

# Generate THIRD_PARTY_LICENSES.md from cargo-about JSON output.
#
# Usage:
#   ./scripts/generate-third-party-licenses.sh [--check]
#
# --check: verify that the file is up to date (for CI / pre-commit).

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/THIRD_PARTY_LICENSES.md"

check=false
if [ "${1:-}" = "--check" ]; then check=true; fi

if ! command -v cargo-about >/dev/null 2>&1; then
  echo "cargo-about is not installed. Run: cargo install cargo-about --features cli" >&2
  exit 1
fi

# The prebuilt binaries are built with --features bundled-typst (wiki/390 §7),
# so the Typst compiler tree is part of what they statically link and must be
# listed. Without the flag cargo-about would only cover the default feature set.
generated="$(cargo-about generate --format json --features bundled-typst --manifest-path "$ROOT/Cargo.toml" 2>/dev/null \
  | python3 "$ROOT/scripts/format-third-party-licenses.py")"

if [ "$check" = true ]; then
  if [ ! -f "$OUT" ]; then
    echo "THIRD_PARTY_LICENSES.md does not exist. Run: ./scripts/generate-third-party-licenses.sh" >&2
    exit 1
  fi
  if ! diff -q <(echo "$generated") "$OUT" >/dev/null 2>&1; then
    echo "THIRD_PARTY_LICENSES.md is out of date. Run: ./scripts/generate-third-party-licenses.sh" >&2
    exit 1
  fi
  echo "THIRD_PARTY_LICENSES.md is up to date."
  exit 0
fi

echo "$generated" > "$OUT"
echo "Generated $OUT ($(wc -l < "$OUT") lines)"
