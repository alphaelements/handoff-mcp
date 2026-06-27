# shellcheck shell=sh
# cargo-env.sh — make cargo commands work across heterogeneous dev environments.
#
# Some environments (e.g. a Docker image where CARGO_HOME=/usr/local/cargo is
# owned by root) expose a CARGO_HOME that the current user cannot write to.
# cargo then fails to download/cache crates or fetch the advisory-db, breaking
# the git hooks even though nothing is wrong with the code.
#
# This script detects an unwritable CARGO_HOME and transparently falls back to
# the per-user $HOME/.cargo, which every supported environment can write to.
# When CARGO_HOME is already writable (local macOS, a synced Mac, or simply
# unset so cargo uses its default ~/.cargo) it leaves the environment untouched.
#
# Usage (from a hook or shell):
#   . scripts/cargo-env.sh && cargo <args>

# Resolve the directory cargo would use right now.
_cargo_home="${CARGO_HOME:-$HOME/.cargo}"

# Probe writability with a temp file; fall back to $HOME/.cargo if we can't write.
if ! ( mkdir -p "$_cargo_home" 2>/dev/null && touch "$_cargo_home/.cargo-env-wtest" 2>/dev/null ); then
  export CARGO_HOME="$HOME/.cargo"
  mkdir -p "$CARGO_HOME" 2>/dev/null || true
else
  rm -f "$_cargo_home/.cargo-env-wtest" 2>/dev/null || true
  export CARGO_HOME="$_cargo_home"
fi

unset _cargo_home
