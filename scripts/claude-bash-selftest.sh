#!/bin/sh
# Run clappa's Bash preload self-test against the `claude` on PATH, the way
# `appa-runtime/src/protected_launch.rs` runs it before a session:
#
#   scripts/claude-bash-selftest.sh <path to appa>
#
# Prints the preload's verdict and exits non-zero unless it is
# `appa-claude-bash: ok`. Needs Claude Code's sandbox on this machine
# (bubblewrap and socat on Linux).
set -eu

appa=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
preload=$(cd "$(dirname "$0")/../appa-file-tracker" && pwd)/claude-bash.js
config=$(mktemp -d)
trap 'rm -rf "$config"' EXIT
printf '%s' '{"sandbox":{"enabled":true,"failIfUnavailable":true,"autoAllowBashIfSandboxed":true,"allowUnsandboxedCommands":false,"network":{"strictAllowlist":true}}}' >"$config/settings.json"

printf 'Claude Code %s\n' "$(claude --version)"
verdict=$(CLAUDE_CONFIG_DIR="$config" BUN_OPTIONS="--preload $preload" APPA_FILE_TRACKER="$appa" \
  APPA_CLAUDE_BASH_SELFTEST=1 timeout 120 claude --version </dev/null | grep '^appa-claude-bash: ' || true)
printf '%s\n' "${verdict:-appa-claude-bash: the self-test did not run}"
[ "$verdict" = "appa-claude-bash: ok" ]
