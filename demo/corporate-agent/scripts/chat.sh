#!/usr/bin/env bash
# Interactive REPL against the corporate systems. Type 'exit' to quit.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

exec cargo run -q --bin corp-agent -- --model "$MODEL" --chat
