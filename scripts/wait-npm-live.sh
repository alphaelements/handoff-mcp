#!/usr/bin/env bash
# Wait until every given package is resolvable at <version> on the npm registry.
#
# Usage: wait-npm-live.sh <version> <package>...
#
# A just-published version can take several minutes to appear (v0.41.0 took
# ~6 min for linux-x64), so all packages share one deadline rather than a fixed
# per-package attempt count. The registry API is queried directly with
# Cache-Control: no-cache; `npm view` can serve a stale cached 404 for the very
# version we are waiting on.
#
# Env (defaults cover ~15 min):
#   NPM_WAIT_TIMEOUT_SECONDS   overall deadline          (default 900)
#   NPM_WAIT_INTERVAL_SECONDS  pause between sweeps      (default 15)
set -euo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <version> <package>..." >&2
  exit 2
fi

version="$1"; shift
timeout="${NPM_WAIT_TIMEOUT_SECONDS:-900}"
interval="${NPM_WAIT_INTERVAL_SECONDS:-15}"
registry="https://registry.npmjs.org"

pending=("$@")
start=$SECONDS

while :; do
  still=()
  for pkg in "${pending[@]}"; do
    if curl -fsS -o /dev/null --max-time 20 \
         -H 'Cache-Control: no-cache' -H 'Pragma: no-cache' \
         "$registry/$pkg/$version" 2>/dev/null; then
      echo "ok: $pkg@$version (after $((SECONDS - start))s)"
    else
      still+=("$pkg")
    fi
  done
  pending=("${still[@]+"${still[@]}"}")
  [ "${#pending[@]}" -eq 0 ] && exit 0
  if [ $((SECONDS - start)) -ge "$timeout" ]; then
    break
  fi
  echo "waiting ($((SECONDS - start))s/${timeout}s): ${pending[*]}"
  sleep "$interval"
done

for pkg in "${pending[@]}"; do
  echo "::error::$pkg@$version is not on the registry after ${timeout}s"
done
exit 1
