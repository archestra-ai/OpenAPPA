#!/bin/bash
# The quickstart pod: appa-runtime beside the gated agent runtime.
#
# Start the bundled runtime on loopback, wait until it answers, then
# exec the gated entrypoint under whatever args the controller sent.
# If the runtime never comes up the pod exits unready — the gate does
# not run without its runtime.
set -euo pipefail

# A deployment that points APPA_RUNTIME_URL at a shared appa-runtime
# gets exactly that: the bundled runtime stays off, and every pod's
# hooks land in one trajectory log. Without it, the pod runs its own
# runtime on loopback — the quickstart default.
if [ -n "${APPA_RUNTIME_URL:-}" ]; then
  echo "quickstart: using the shared appa runtime at ${APPA_RUNTIME_URL}" >&2
  exec appa-kagent-adk "$@"
fi

APPA_LISTEN="127.0.0.1:8787"

echo "quickstart: starting appa runtime on ${APPA_LISTEN} (policy: ${APPA_CONFIG})" >&2
appa runtime --adapter kagent --listen "${APPA_LISTEN}" --config "${APPA_CONFIG}" --db "${APPA_DB}" &
runtime_pid=$!

for attempt in $(seq 1 60); do
  if curl -fsS "http://${APPA_LISTEN}/health" >/dev/null 2>&1; then
    break
  fi
  if ! kill -0 "${runtime_pid}" 2>/dev/null; then
    echo "quickstart: appa runtime exited during startup" >&2
    exit 1
  fi
  sleep 0.5
done
if ! curl -fsS "http://${APPA_LISTEN}/health" >/dev/null 2>&1; then
  echo "quickstart: appa runtime did not become healthy" >&2
  exit 1
fi

export APPA_RUNTIME_URL="http://${APPA_LISTEN}"
echo "quickstart: appa runtime is healthy; starting the gated agent runtime" >&2
exec appa-kagent-adk "$@"
