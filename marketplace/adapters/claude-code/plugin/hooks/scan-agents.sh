#!/bin/sh
# Refuses a prompt while a subagent definition in reach declares `maxTurns`.
# Claude Code ends such a subagent at its turn cap with no SubagentStop, so
# the return check never runs and the parent receives the subagent's partial
# output unchecked. The project and user agent directories and the installed
# plugins' agent directories hold the definitions a session can start;
# agents passed on the command line (`--agents`, `--plugin-dir`) are not
# scanned. Exit 2 blocks the prompt, and the reason on stderr reaches the
# user.
set -u

found=""
declaring() {
  [ -f "$1" ] && grep -q '^maxTurns:' "$1"
}
for dir in "${CLAUDE_PROJECT_DIR:-$PWD}/.claude/agents" "$HOME/.claude/agents"; do
  [ -d "$dir" ] || continue
  for file in "$dir"/*.md; do
    if declaring "$file"; then
      found="$found
  $file"
    fi
  done
done
if [ -d "$HOME/.claude/plugins/cache" ]; then
  # find hands grep whole pathnames, so a space in a plugin or agent name is safe.
  plugins=$(find "$HOME/.claude/plugins/cache" -path '*/agents/*.md' -exec grep -l '^maxTurns:' {} + 2>/dev/null | awk '{ print "  " $0 }')
  if [ -n "$plugins" ]; then
    found="$found
$plugins"
  fi
fi
[ -z "$found" ] && exit 0

echo "[appa] this session cannot be protected while a subagent definition declares maxTurns: Claude Code ends that subagent without the return check and hands the parent its partial output unchecked. Remove maxTurns from:$found" >&2
exit 2
