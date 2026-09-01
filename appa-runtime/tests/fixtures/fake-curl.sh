#!/bin/sh
# The runtime's answers, by route. FAKE_RUNTIME_FINGERPRINT_LATER, when set, is
# what every /binary-fingerprint call after the first reports: init probes the
# endpoint once before it mutates anything and once after the start, and the two
# answers differing is how a foreign runtime arriving mid-install is reproduced.
set -eu

case "$*" in
  *"/health"*)
    printf 'ok\n'
    exit 0
    ;;
  *"/policy-key"*)
    # A runtime that does not answer for its policy, which is what curl --fail
    # reports as a failure and init reads as nothing to reconcile. Set
    # FAKE_POLICY_KEY to give the endpoint a policy to serve instead.
    if [ -z "${FAKE_POLICY_KEY:-}" ]; then
      exit 22
    fi
    printf '%s\n' "$FAKE_POLICY_KEY"
    exit 0
    ;;
  *"/reload"*)
    # A successful reload, which is all init reads: the body is not consumed, so
    # the fixture invents none. FAKE_RELOADS, when set, records that init got
    # this far.
    if [ -n "${FAKE_RELOADS:-}" ]; then
      printf 'reload\n' >>"$FAKE_RELOADS"
    fi
    exit 0
    ;;
esac

if [ -n "${FAKE_CURL_CALLS:-}" ]; then
  count=$(( $(cat "$FAKE_CURL_CALLS" 2>/dev/null || echo 0) + 1 ))
  printf '%s' "$count" >"$FAKE_CURL_CALLS"
  if [ -n "${FAKE_RUNTIME_FINGERPRINT_LATER:-}" ] && [ "$count" -gt 1 ]; then
    printf '%s\n%s\n' "$FAKE_RUNTIME_FINGERPRINT_LATER" "$FAKE_RUNTIME_CONFIG"
    exit 0
  fi
fi

# A deployment is the build and the configuration it serves, each on its own
# line. Both are mandatory: a caller that names only the build has not said which
# deployment answers, and `set -u` refuses rather than answering for a nameless one.
printf '%s\n%s\n' "$FAKE_RUNTIME_FINGERPRINT" "$FAKE_RUNTIME_CONFIG"
