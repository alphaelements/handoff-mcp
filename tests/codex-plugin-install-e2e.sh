#!/usr/bin/env bash
# Install the Codex Task Loop from this checkout into an empty Codex profile.
# This is intentionally local-source E2E coverage: GitHub-source coverage must
# run after the release containing the marketplace files is published.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CODEX_BIN="${CODEX_BIN:-codex}"
PLUGIN="handoff-task-loop-codex"
MARKETPLACE="handoff-mcp-marketplace"
TEST_PROFILE="$(mktemp -d "$ROOT/tmp/codex-plugin-e2e.XXXXXX")"

cleanup() {
  rm -rf "$TEST_PROFILE"
}
trap cleanup EXIT

run_codex() {
  env CODEX_HOME="$TEST_PROFILE" "$CODEX_BIN" "$@"
}

run_codex plugin marketplace add "$ROOT" --json
run_codex plugin add "$PLUGIN@$MARKETPLACE"

plugin_list="$(run_codex plugin list)"
printf '%s\n' "$plugin_list"
grep -Fq "$PLUGIN@$MARKETPLACE" <<<"$plugin_list"
grep -Fq "installed, enabled" <<<"$plugin_list"

version="$(node -p "require('$ROOT/package.json').version")"
installed_root="$TEST_PROFILE/plugins/cache/$MARKETPLACE/$PLUGIN/$version"
test -f "$installed_root/.codex-plugin/plugin.json"
test -f "$installed_root/skills/handoff-session-loop/SKILL.md"
test -f "$installed_root/skills/handoff-session-loop/agents/openai.yaml"

for role in manager developer tester reviewer; do
  test -f "$installed_root/skills/handoff-session-loop/references/$role.md"
done

node - "$installed_root" "$version" <<'NODE'
const fs = require('node:fs');
const [root, version] = process.argv.slice(2);
const manifest = JSON.parse(fs.readFileSync(`${root}/.codex-plugin/plugin.json`, 'utf8'));
if (manifest.name !== 'handoff-task-loop-codex') throw new Error('unexpected plugin name');
if (manifest.version !== version) throw new Error('installed plugin version does not match package.json');
const metadata = fs.readFileSync(`${root}/skills/handoff-session-loop/agents/openai.yaml`, 'utf8');
if (!metadata.includes('allow_implicit_invocation: false')) throw new Error('skill must remain explicit-only');
console.log('Codex Task Loop clean-profile installation: passed');
NODE
