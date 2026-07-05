#!/usr/bin/env bash
# Install handoff-mcp locally: binary, skills, and plugin caches.
# Usage: ./scripts/install-local.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"
CLAUDE_DIR="${HOME}/.claude"
CACHE_DIR="${CLAUDE_DIR}/plugins/cache/handoff-mcp-marketplace"
INSTALLED_JSON="${CLAUDE_DIR}/plugins/installed_plugins.json"

cd "$ROOT"

# ---------- 1. Binary ----------
echo "==> Building release binary..."
cargo build --release

mkdir -p "${HOME}/.local/bin"
rm -f "${HOME}/.local/bin/handoff-mcp"
cp target/release/handoff-mcp "${HOME}/.local/bin/handoff-mcp"
echo "    Installed: $(${HOME}/.local/bin/handoff-mcp --version)"

# ---------- 2. Bundled skills ----------
echo "==> Syncing skills..."
skill_count=0
for skill in skills/*/; do
  name=$(basename "$skill")
  mkdir -p "${CLAUDE_DIR}/skills/${name}"
  cp "${skill}SKILL.md" "${CLAUDE_DIR}/skills/${name}/SKILL.md"
  skill_count=$((skill_count + 1))
done
echo "    ${skill_count} skills synced"

# ---------- 3. Sync plugin distribution skills ----------
echo "==> Syncing plugin distribution skills..."
"${SCRIPT_DIR}/sync-plugin-skills.sh"

# ---------- 4. Plugin caches ----------
echo "==> Syncing plugin caches..."

sync_plugin() {
  local src_dir="$1"
  local plugin_json="${src_dir}/.claude-plugin/plugin.json"

  if [ ! -f "$plugin_json" ]; then
    echo "    SKIP: ${src_dir} (no .claude-plugin/plugin.json)"
    return
  fi

  local name version cache_dest
  name=$(python3 -c "import json,sys; print(json.load(sys.stdin)['name'])" < "$plugin_json")
  version=$(python3 -c "import json,sys; print(json.load(sys.stdin)['version'])" < "$plugin_json")
  cache_dest="${CACHE_DIR}/${name}/${version}"

  rm -rf "$cache_dest"
  mkdir -p "$cache_dest"

  # Copy everything except .claude-plugin/ first, then copy .claude-plugin/
  find "$src_dir" -mindepth 1 -maxdepth 1 -not -name '.claude-plugin' -exec cp -a {} "$cache_dest/" \;
  cp -a "${src_dir}/.claude-plugin" "$cache_dest/.claude-plugin"

  echo "    ${name}@${version} -> ${cache_dest}"

  # Update installed_plugins.json timestamp
  if [ -f "$INSTALLED_JSON" ] && command -v python3 >/dev/null 2>&1; then
    local git_sha
    git_sha=$(git rev-parse HEAD 2>/dev/null || echo "unknown")
    local now
    now=$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")
    python3 -c "
import json, sys
key = '${name}@handoff-mcp-marketplace'
with open('${INSTALLED_JSON}', 'r') as f:
    data = json.load(f)
if key in data.get('plugins', {}):
    for entry in data['plugins'][key]:
        entry['installPath'] = '${cache_dest}'
        entry['version'] = '${version}'
        entry['lastUpdated'] = '${now}'
        entry['gitCommitSha'] = '${git_sha}'
    with open('${INSTALLED_JSON}', 'w') as f:
        json.dump(data, f, indent=4)
        f.write('\n')
" 2>/dev/null || true
  fi
}

sync_plugin "${ROOT}/plugin"
sync_plugin "${ROOT}/plugin-hooks"
sync_plugin "${ROOT}/plugin-task-loop"

# ---------- 5. Verify ----------
echo ""
echo "==> Done. Restart Claude Code to pick up changes."
echo "    Binary:  ${HOME}/.local/bin/handoff-mcp"
echo "    Skills:  ${CLAUDE_DIR}/skills/"
echo "    Plugins: ${CACHE_DIR}/"
