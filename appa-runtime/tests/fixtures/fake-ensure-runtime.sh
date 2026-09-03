#!/bin/sh
# The installed plugin's starter, as init runs it. Exits 0 with nothing started
# by default, because fake-curl answers as a healthy runtime anyway.
#
# FAKE_STARTER_FAILS makes the start fail, which is what a runtime that never
# became healthy looks like to init.
#
# FAKE_RUNTIME_STAND_IN names a directory: a perl copy named `appa` is started
# there, sleeping, detached from this script's pipes so init's wait on the
# starter returns, and its pid is written to `$FAKE_RUNTIME_STAND_IN/pid` for
# fake-curl to report. It passes init's same-user, process-name ownership check,
# so it is what init stops when it rolls a failed install back.
set -eu

if [ -n "${FAKE_STARTER_FAILS:-}" ]; then
  printf 'deliberate fake starter failure\n' >&2
  exit 1
fi

if [ -n "${FAKE_RUNTIME_STAND_IN:-}" ] && [ ! -f "$FAKE_RUNTIME_STAND_IN/pid" ]; then
  perl=$(command -v perl) || {
    printf 'the runtime stand-in needs perl\n' >&2
    exit 1
  }
  mkdir -p "$FAKE_RUNTIME_STAND_IN"
  cp "$perl" "$FAKE_RUNTIME_STAND_IN/appa"
  chmod 755 "$FAKE_RUNTIME_STAND_IN/appa"
  "$FAKE_RUNTIME_STAND_IN/appa" -e 'sleep 300' </dev/null >/dev/null 2>&1 &
  printf '%s\n' "$!" >"$FAKE_RUNTIME_STAND_IN/pid"
fi
exit 0
