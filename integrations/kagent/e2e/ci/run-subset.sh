#!/usr/bin/env bash
# Runs the live subset against the installed demo stack: five of the
# A2A cases, on the real model, through the parent agent alone. It
# port-forwards the parent Service and the mocks, runs the cases, and
# appends the model and the timings to
# $GITHUB_STEP_SUMMARY when a runner sets it. It exits with pytest's
# status.
#
#   ./run-subset.sh
#
# Env, with defaults:
#   APPA_E2E_NAMESPACE   kagent       the release namespace
#   APPA_E2E_AGENT       cluster-ops  the parent under test
#   APPA_E2E_AGENT_PORT  18089        the local port for that agent
#   APPA_E2E_MOCK_PORT   8081         the local port for the mocks
#   APPA_E2E_CASES                    the pytest -k expression
#   APPA_E2E_MODEL                    reported, not applied
#   APPA_E2E_BASE_URL                 reported, not applied
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
a2a=$(cd "$here/../a2a" && pwd)

namespace=${APPA_E2E_NAMESPACE:-kagent}
agent=${APPA_E2E_AGENT:-cluster-ops}
agent_port=${APPA_E2E_AGENT_PORT:-18089}
mock_port=${APPA_E2E_MOCK_PORT:-8081}
model=${APPA_E2E_MODEL:-openai/gpt-5.6-luna}
base_url=${APPA_E2E_BASE_URL:-https://openrouter.ai/api/v1}
# One case each: the allowed read, the exfiltration ask, the agent's
# configured remedy, the approved human review, and the delegated child.
cases=${APPA_E2E_CASES:-"ordinary_read or exfil or configured_default or approval_runs or delegated_child"}

work=$(mktemp -d)
log="$work/pytest.log"
agent_url="http://127.0.0.1:$agent_port/"
mock_url="http://127.0.0.1:$mock_port"

# The port-forwards and the work directory live as long as the run.
forwards=()
trap 'kill "${forwards[@]:-}" 2>/dev/null || true; rm -rf "$work"' EXIT

forward() {
  local service=$1 local_port=$2 remote_port=$3
  kubectl -n "$namespace" port-forward "svc/$service" "$local_port:$remote_port" \
    >>"$work/port-forward.log" 2>&1 &
  forwards+=("$!")
}

# The forward answers as soon as kubectl holds the connection. Any HTTP
# status proves that, so the poll asks for a reply and not for a 200.
wait_for_url() {
  local url=$1 label=$2
  for _ in $(seq 1 30); do
    if curl -sS -o /dev/null --max-time 5 "$url"; then
      return 0
    fi
    sleep 2
  done
  echo "run-subset: $label never answered at $url" >&2
  cat "$work/port-forward.log" >&2
  return 1
}

echo "== port-forward svc/$agent $agent_port:8080 and svc/appa-demo-mocks $mock_port:8081"
forward "$agent" "$agent_port" 8080
forward appa-demo-mocks "$mock_port" 8081
wait_for_url "$mock_url/healthz" "the mocks"
wait_for_url "$agent_url" "$agent"

echo "== five A2A cases on $model at $base_url"
started=$(date +%s)
status=0
(
  cd "$a2a"
  # Every selected case gets three total attempts. Per-test flaky markers
  # use the same floor for the full matrix outside this subset.
  APPA_A2A_E2E=1 APPA_A2A_URL="$agent_url" APPA_MOCK_URL="$mock_url" \
    uv run --with "pytest>=8" --with pytest-rerunfailures \
    pytest -v -rA --durations=0 --reruns 2 -k "$cases" .
) 2>&1 | tee "$log" || status=$?
seconds=$(( $(date +%s) - started ))
echo "== pytest exited $status after ${seconds}s"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### kagent live subset"
    echo
    echo "| Field | Value |"
    echo "|---|---|"
    echo "| agent | \`$agent\` in namespace \`$namespace\` |"
    echo "| model | \`$model\` |"
    echo "| endpoint | \`$base_url\` |"
    echo "| cases | \`$cases\` |"
    echo "| pytest exit | \`$status\` |"
    echo "| seconds | \`$seconds\` |"
    echo
    echo '```'
    if grep -q 'short test summary info' "$log"; then
      sed -n '/short test summary info/,$p' "$log"
    else
      tail -n 20 "$log"
    fi
    echo '```'
  } >>"$GITHUB_STEP_SUMMARY"
fi

exit "$status"
