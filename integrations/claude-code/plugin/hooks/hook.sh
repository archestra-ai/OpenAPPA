#!/bin/sh
# Post one hook event through the appa binary this deployment installed.
#
# A dumb invoker, deliberately: it reads no event name and makes no decision.
# Each event's guard, blocking exit code and timeout stay in hooks.json, beside
# the event they are registered for.
set -eu

# shellcheck source=integrations/claude-code/plugin/hooks/appa-paths.sh
. "$(dirname "$0")/appa-paths.sh"

exec "$APPA_BIN" hook --url "${APPA_RUNTIME_URL:-$APPA_ENDPOINT}" "$@"
