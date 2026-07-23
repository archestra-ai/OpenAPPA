#!/usr/bin/env bash
# Interactive REPL against the corporate systems, mediated by APPA. Type 'exit' to quit.
#
# The policy is bound for the whole session. Pass one as the first argument to switch:
#   ./scripts/chat.sh                        # guarded appa-policy.toml (default) — blocks the leak
#   ./scripts/chat.sh appa-policy-open.toml  # the unmediated contrast   — lets it leak
# (or set APPA_DEMO_POLICY). Restart to change policy; it cannot change mid-chat.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

POLICY="${1:-${APPA_DEMO_POLICY:-appa-policy.toml}}"

exec cargo run -q --bin corp-agent -- --model "$MODEL" --policy "$POLICY" --chat
