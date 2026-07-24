#!/usr/bin/env bash
# Interactive REPL against the FIDES-mediated corporate agent.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"
run_agent --chat
