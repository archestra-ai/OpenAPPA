#!/bin/sh
# Post one hook event through the appa binary this deployment installed.
#
# A dumb invoker, deliberately: it reads no event name and makes no decision.
# Each event's guard, blocking exit code and timeout stay in hooks.json, beside
# the event they are registered for.
set -eu

# Parameter expansion rather than `dirname`: this runs on every gated tool call,
# and a command substitution would fork a shell and exec a binary each time.
case "$0" in
  */*) appa_hooks_dir=${0%/*} ;;
  *) appa_hooks_dir=. ;;
esac
# shellcheck source=integrations/claude-code/plugin/hooks/appa-paths.sh
. "$appa_hooks_dir/appa-paths.sh"

exec "$APPA_BIN" hook --adapter claude-code --url "${APPA_RUNTIME_URL:-$APPA_ENDPOINT}" "$@"
