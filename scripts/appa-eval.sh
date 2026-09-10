#!/bin/sh
# Repository evaluation entry point for developers and coding agents.
set -u

repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
CDPATH='' cd -- "$repo" || exit 1
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
profile=full
live_model=0

usage() {
  echo "usage: scripts/appa-eval.sh [--quick] [--live-model]" >&2
  echo "  --quick       omit slower lint, website, and kagent integration checks" >&2
  echo "  --live-model  also run the paid Claude compatibility canary" >&2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --quick) profile=quick ;;
    --live-model) live_model=1 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
  shift
done

passed=0
failed=0
failed_names=

# Package identities cover every file in their trees. Remove Python's ignored
# bytecode caches so a previous test run cannot change a package's identity.
find marketplace -type d -name __pycache__ -prune -exec rm -rf -- {} +

run() {
  name=$1
  shift
  printf '\n==> %s\n' "$name"
  if "$@"; then
    passed=$((passed + 1))
  else
    status=$?
    failed=$((failed + 1))
    failed_names="${failed_names}\n  - ${name} (exit ${status})"
  fi
}

run_at() {
  directory=$1
  name=$2
  shift 2
  printf '\n==> %s\n' "$name"
  if (CDPATH='' cd -- "$directory" && "$@"); then
    passed=$((passed + 1))
  else
    status=$?
    failed=$((failed + 1))
    failed_names="${failed_names}\n  - ${name} (exit ${status})"
  fi
}

run "Rust formatting" cargo fmt --all --check
if [ "$profile" = full ]; then
  run "Rust lint" cargo clippy --workspace --all-targets --locked -- -D warnings
fi
run "Rust tests" cargo test --workspace --locked
run "Runtime binary" cargo build --package appa --locked

run "Repository Python tests" uv run --with 'pyyaml==6.0.2' python3 -m unittest \
  scripts/test_appa_refresh_batteries.py \
  scripts/test_appa_guide_runtime.py \
  scripts/test_appa_oci_tags.py \
  scripts/test_appa_image_descriptor.py
run "Claude model fixture tests" python3 -m unittest discover \
  -s marketplace/plugins/claude-code -p 'test_*.py'
run "kagent Python unit tests" uv run --project integrations/kagent/appa-kagent-adk \
  pytest integrations/kagent/appa-kagent-adk/tests
run_at "$repo/integrations/kagent/appa-kagent-adk-go" "kagent Go unit tests" go test ./...

if [ "$profile" = full ]; then
  run "kagent deterministic integration" env \
    APPA_INTEGRATION=1 APPA_BIN="$repo/target/debug/appa" \
    uv run --project integrations/kagent/appa-kagent-adk \
    --with 'kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk' \
    --with 'a2a-sdk>=0.3.23,<0.4' --with 'google-adk==1.31.1' --with 'mcp>=1.25,<2' \
    --with 'pytest>=8' --with pytest-asyncio pytest integrations/kagent/tests -q
  run_at "$repo/website" "Website dependencies" pnpm install --frozen-lockfile
  run_at "$repo/website" "Website typecheck" pnpm typecheck
  run_at "$repo/website" "Website build" pnpm build
fi

run "Claude Code deterministic harness" python3 \
  marketplace/plugins/claude-code/live-gate-check.py --model scripted
if [ "$live_model" -eq 1 ]; then
  run "Claude Code live-model canary" python3 \
    marketplace/plugins/claude-code/live-gate-check.py --model live
fi

printf '\nEvaluation: %s passed, %s failed (%s profile)\n' "$passed" "$failed" "$profile"
if [ "$failed" -ne 0 ]; then
  printf 'Failures:%b\n' "$failed_names"
  exit 1
fi
