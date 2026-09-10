#!/bin/sh
# The APPA statusline: the pixel mascot, plus the root trajectory's current
# Trust, Audience, and direct APPA-authored share of Claude's input context.
#
# The mark is the website mascot as half blocks — solid pixels, eyes
# as terminal-background gaps, like the SVG. The chips render the
# statusline's stdin `session_id` mapped to the trajectory the Claude
# Code adapter names, `cc:<session_id>`.
#
# In a protected session the statusline fails open, the opposite of the
# hooks' `|| exit 2`: every failure — runtime down, unknown trajectory,
# missing jq or curl, malformed stdin — prints the mascot alone and exits 0.
#
# An unprotected session (no APPA_GATE=1) is completely silent, so the
# global Claude statusline setting changes only sessions launched by clappa.
if [ "${APPA_GATE:-}" != 1 ]; then
  exit 0
fi
input=$(cat)
chips=''
if command -v jq >/dev/null 2>&1 && command -v curl >/dev/null 2>&1; then
  sid=$(printf '%s' "$input" | jq -er '.session_id' 2>/dev/null) &&
    body=$(curl -sf --connect-timeout 0.1 -m 0.3 --get \
      --data-urlencode "trajectory=cc:${sid}" \
      "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/status" 2>/dev/null) &&
    chips=$(printf '%s' "$body" |
      jq -er --argjson total "$(printf '%s' "$input" | jq '.context_window.total_input_tokens // 0')" '
        select((.trust | type) == "string" and (.audience | type) == "string" and (.appa_tokens | type) == "number")
        | if $total > 0 then
            "trust:\(.trust) · audience:\(.audience) · \u001b[2m~\(.appa_tokens) APPA tokens (\((.appa_tokens * 1000 / $total | round) / 10)%)\u001b[0m"
          else
            "trust:\(.trust) · audience:\(.audience) · \u001b[2m~\(.appa_tokens) APPA tokens\u001b[0m"
          end' 2>/dev/null) ||
    chips=''
fi
if [ -n "$chips" ]; then
  printf '▄█▄▄▄█▄  %s\n██▄█▄██\n' "$chips"
else
  printf '▄█▄▄▄█▄\n██▄█▄██\n'
fi
exit 0
