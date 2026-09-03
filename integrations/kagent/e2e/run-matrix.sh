#!/usr/bin/env bash
# Runs one row of the kagent end-to-end matrix, or every row that runs
# today: <runtime> is python or go, <driver> is ui or a2a.
#
#   ./run-matrix.sh python ui | python a2a | go ui | go a2a | all
#
# The dashboard driver needs the UI at $APPA_UI_URL (default
# http://127.0.0.1:8901); the A2A driver needs the agent under test
# port-forwarded to $APPA_A2A_URL (defaults: cluster-ops on 18089,
# cluster-ops-go on 18090). Both need the mocks' side channel at
# $APPA_MOCK_URL (default http://127.0.0.1:8081).
set -euo pipefail
cd "$(dirname "$0")"

row() {
  local runtime=$1 driver=$2 agent url
  case "$runtime" in
    python) agent=cluster-ops;    url=${APPA_A2A_URL:-http://127.0.0.1:18089/} ;;
    go)     agent=cluster-ops-go; url=${APPA_A2A_URL:-http://127.0.0.1:18090/} ;;
    *) echo "unknown runtime: $runtime (python|go)" >&2; exit 2 ;;
  esac
  echo "== kagent v0.9.12 · $runtime ($agent) · $driver"
  case "$driver" in
    ui)  (cd ui  && APPA_UI_E2E=1  APPA_AGENT=$agent uv run --with playwright --with "pytest>=8" --with pytest-rerunfailures pytest -v .) ;;
    a2a) (cd a2a && APPA_A2A_E2E=1 APPA_A2A_URL=$url uv run --with "pytest>=8" --with pytest-rerunfailures pytest -v .) ;;
    *) echo "unknown driver: $driver (ui|a2a)" >&2; exit 2 ;;
  esac
}

if [ "${1:-}" = all ]; then
  for runtime in python go; do for driver in a2a ui; do row "$runtime" "$driver"; done; done
else
  [ $# -eq 2 ] || { echo "usage: $0 <python|go> <ui|a2a> | all" >&2; exit 2; }
  row "$1" "$2"
fi
