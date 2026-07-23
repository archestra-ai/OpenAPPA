#!/usr/bin/env bash
# Shared setup for the corp-agent scenario scripts. `source` this — don't run it.
#
# It cd's to the crate root, loads `.env` (so APPA_DEMO_MODEL / OPENROUTER_API_KEY
# are available here and to the binary), builds both binaries once, and defines
# the `run_agent` / `reset_email` / `show_email` helpers the scenarios use.
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$CRATE_DIR"

# Load the crate-local .env for this shell too (the agent also loads it itself).
if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source ./.env
  set +a
fi

# Default model — any valid OpenRouter id; override via APPA_DEMO_MODEL or .env.
MODEL="${APPA_DEMO_MODEL:-openai/gpt-5.6-luna}"

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
  echo "warning: OPENROUTER_API_KEY is not set — copy .env.example to .env and add your key." >&2
fi

echo "· building corp-agent + corp-systems-mcp (model: $MODEL)…" >&2
cargo build -q

# run_agent <prompt> [extra corp-agent flags...]
run_agent() {
  cargo run -q --bin corp-agent -- --model "$MODEL" "$@"
}

# reset_email — clear the send_email sink so a run starts clean.
reset_email() {
  rm -f "$CRATE_DIR/data/email/"*.md 2>/dev/null || true
}

# show_email — print whatever landed in the send_email sink.
show_email() {
  echo
  echo "=== data/email (send_email sink) ==="
  if compgen -G "$CRATE_DIR/data/email/*.md" >/dev/null; then
    for f in "$CRATE_DIR/data/email/"*.md; do
      echo "--- $f ---"
      cat "$f"
      echo
    done
  else
    echo "(empty — nothing was emailed)"
  fi
}
