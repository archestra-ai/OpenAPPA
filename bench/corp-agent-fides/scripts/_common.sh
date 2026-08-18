#!/usr/bin/env bash
# Shared setup for the corp-agent-fides scenario scripts. `source` this — don't run it.
#
# It cd's to the crate root, loads `.env` (so FIDES_DEMO_MODEL / OPENROUTER_API_KEY
# are available here and to the module), builds the shared `corp-systems-mcp`
# server (the sibling Rust crate this demo spawns), and defines the `run_agent` /
# `reset_email` / `show_email` helpers the scenarios use. The corpus is the
# sibling `corp-systems/data`; the `send_email` sink is this demo's own
# `data/email/`.
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$CRATE_DIR"

echo "· building corp-systems-mcp (the shared MCP server)…" >&2
cargo build -q --manifest-path "$CRATE_DIR/../corp-systems/Cargo.toml"

if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source ./.env
  set +a
fi

# Default model — any valid OpenRouter id; override via FIDES_DEMO_MODEL or .env.
MODEL="${FIDES_DEMO_MODEL:-anthropic/claude-sonnet-5}"

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
  echo "warning: OPENROUTER_API_KEY is not set — copy .env.example to .env and add your key." >&2
fi

# Prefer the installed console script; fall back to the module for a bare checkout.
if command -v corp-agent-fides >/dev/null 2>&1; then
  AGENT=(corp-agent-fides)
else
  AGENT=(python3 -m corp_fides)
fi

# run_agent <prompt> [extra flags...]
run_agent() {
  "${AGENT[@]}" --model "$MODEL" "$@"
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
