#!/bin/bash
# The quickstart pod: appa-runtime beside the agent runtime.
#
# APPA_ENABLED is the one knob, and it is off by default. Off execs the
# wrapped runtime and starts nothing else, so the pod serves exactly
# what the stock kagent runtime serves. On starts the bundled runtime on
# loopback, waits until it answers, then execs the gated runtime under
# whatever args the controller sent. If the runtime never comes up the
# pod exits unready, because the gate does not run without its runtime.
set -euo pipefail

# The knob has a closed value set: unset, empty and "false" are off, and
# "true" is on. The two expansions cut leading and trailing whitespace,
# and the case word lowers the value. Any other value refuses the start,
# so a typo never disables the gate in silence.
appa_enabled="${APPA_ENABLED:-}"
appa_enabled="${appa_enabled#"${appa_enabled%%[![:space:]]*}"}"
appa_enabled="${appa_enabled%"${appa_enabled##*[![:space:]]}"}"
case "${appa_enabled,,}" in
  "" | false)
    appa_mode="off"
    ;;
  true)
    appa_mode="on"
    ;;
  *)
    echo "quickstart: refusing to start: APPA_ENABLED accepts \"true\", \"false\" or no value. The value is \"${APPA_ENABLED}\"" >&2
    exit 2
    ;;
esac

# Off is the default, and it is what an operator gets by pointing kagent
# at this image. The wrapped runtime reads the same knob and builds the
# stock server, so this branch only skips the bundled runtime.
# APPA_RUNTIME_URL stays ignored, and the wrapped runtime names that
# mistake in its own startup line.
if [ "${appa_mode}" = "off" ]; then
  echo "quickstart: APPA_ENABLED is not true. This pod runs the stock kagent runtime UNGATED and starts no appa runtime" >&2
  exec appa-kagent-adk "$@"
fi

# A deployment that points APPA_RUNTIME_URL at a shared appa-runtime
# gets exactly that: the bundled runtime stays off, and every pod's
# hooks land in one trajectory log. Without it, the pod runs its own
# runtime on loopback — the quickstart default.
if [ -n "${APPA_RUNTIME_URL:-}" ]; then
  echo "quickstart: APPA_ENABLED is true. This pod uses the shared appa runtime at ${APPA_RUNTIME_URL}" >&2
  exec appa-kagent-adk "$@"
fi

APPA_LISTEN="127.0.0.1:8787"

echo "quickstart: APPA_ENABLED is true. This pod starts the bundled appa runtime on ${APPA_LISTEN} (policy: ${APPA_CONFIG})" >&2
appa runtime --adapter kagent --listen "${APPA_LISTEN}" --config "${APPA_CONFIG}" --db "${APPA_DB}" &
runtime_pid=$!

# The loop counts the attempts and needs no variable of its own.
for _ in $(seq 1 60); do
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
