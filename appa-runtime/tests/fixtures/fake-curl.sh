#!/bin/sh
# The runtime's /binary-fingerprint answer. FAKE_RUNTIME_FINGERPRINT_LATER, when
# set, is what every call after the first reports: init probes the endpoint once
# before it mutates anything and once after the start, and the two answers
# differing is how a foreign runtime arriving mid-install is reproduced.
set -eu

case "$*" in
  *"/health"*)
    printf 'ok\n'
    exit 0
    ;;
esac

if [ -n "${FAKE_CURL_CALLS:-}" ]; then
  count=$(( $(cat "$FAKE_CURL_CALLS" 2>/dev/null || echo 0) + 1 ))
  printf '%s' "$count" >"$FAKE_CURL_CALLS"
  if [ -n "${FAKE_RUNTIME_FINGERPRINT_LATER:-}" ] && [ "$count" -gt 1 ]; then
    printf '%s\n' "$FAKE_RUNTIME_FINGERPRINT_LATER"
    exit 0
  fi
fi

printf '%s\n' "$FAKE_RUNTIME_FINGERPRINT"
