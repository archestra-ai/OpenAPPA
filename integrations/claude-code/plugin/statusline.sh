#!/bin/sh
# The APPA statusline: the pixel mascot, plus the root trajectory's
# current Trust and Audience from the runtime's /status read.
#
# The mark is the website mascot as half blocks — solid pixels, eyes
# as terminal-background gaps, like the SVG. The chips render the
# statusline's stdin `session_id` mapped to the trajectory the Claude
# Code adapter names, `cc:<session_id>`.
#
# A statusline fails open, the opposite of the hooks' `|| exit 2`:
# every failure — runtime down, unknown trajectory, missing jq or
# curl, malformed stdin — prints the mascot alone and exits 0.
#
# An ungated session (no APPA_GATE=1) never queries the runtime; it
# prints the mascot with a reminder that clappa starts the gate.
input=$(cat)
if [ "${APPA_GATE:-}" != 1 ]; then
  printf '▄█▄▄▄█▄  ungated — run clappa to gate\n██▄█▄██\n'
  exit 0
fi
chips=''
if command -v jq >/dev/null 2>&1 && command -v curl >/dev/null 2>&1; then
  sid=$(printf '%s' "$input" | jq -er '.session_id' 2>/dev/null) &&
    body=$(curl -sf --connect-timeout 0.1 -m 0.3 --get \
      --data-urlencode "trajectory=cc:${sid}" \
      "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/status" 2>/dev/null) &&
    chips=$(printf '%s' "$body" |
      jq -er 'select((.trust | type) == "string" and (.audience | type) == "string")
        | "trust:\(.trust)  audience:\(.audience)"' 2>/dev/null) ||
    chips=''
fi
if [ -n "$chips" ]; then
  printf '▄█▄▄▄█▄  %s\n██▄█▄██\n' "$chips"
else
  printf '▄█▄▄▄█▄\n██▄█▄██\n'
fi
exit 0
