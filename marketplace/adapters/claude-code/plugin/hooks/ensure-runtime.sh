#!/bin/sh
# Starts the installed `appa runtime` process when no healthy runtime answers.
# Two callers share it: `appa init claude-code` and every protected
# SessionStart, which runs it before the hooks post
# their first event. A protected session therefore needs no login service,
# and an install that ends here leaves the runtime up, so the first
# protected session pays nothing for the start. Exit 0 means a healthy
# runtime answers; any other exit makes the chained protection hook block,
# and tells the install that it has nothing to report as running.
# Installing the binary is not this script's job: `appa init claude-code`
# installs it before registering this plugin, and renders its absolute path
# into appa-paths.sh beside this file. Nothing here consults PATH.
#
# Concurrent starts need no lock. The runtime binds the loopback port, so
# the first process to bind serves and every later one exits at once with
# "address already in use"; both callers then see the same healthy
# runtime. The port is a mutex that cannot go stale, while a lock file
# outlives the hook that Claude Code kills at its timeout and would block
# every start after it.
set -u

# shellcheck source=marketplace/adapters/claude-code/plugin/hooks/appa-paths.sh
. "$(dirname "$0")/appa-paths.sh"

# APPA_RUNTIME_URL keeps both of its jobs: the URL a client talks to, and the
# signal that the runtime answering it is the user's own to restart.
runtime_url=${APPA_RUNTIME_URL:-$APPA_ENDPOINT}

# One cheap probe. A dead loopback port refuses at once on most systems but
# hangs under some network stacks (WSL2 mirrored networking), where the
# deadline is what the probe actually costs.
probe() {
  curl -sf -m 1 "$runtime_url/health" 2>/dev/null || true
}

# A runtime the user runs at a URL of their own (APPA_RUNTIME_URL, the
# development setup) is theirs to restart: it is healthy while it
# answers, stale or not. The starter replaces only the default deployment.
healthy() {
  case $(probe) in
    ok) return 0 ;;
    stale\ *) [ -n "${APPA_RUNTIME_URL:-}" ] ;;
    *) return 1 ;;
  esac
}

# A runtime answers `stale <pid>` once an install replaced its binary on
# disk: the process still serves the build it started from. Stopping it
# here makes the install take effect, at the cost of the protected
# sessions already open, whose hooks fail closed until the start below
# answers. The pid arrives in an HTTP body from whoever holds the port,
# so only this user's own appa process is ever signalled.
# Returns 0 once the port refuses, which is the start's normal starting
# point; exits 0 itself when another starter has already replaced the
# runtime, and 1 when the stale runtime cannot be stopped.
stop_stale_runtime() {
  case $1 in
    '' | 0* | *[!0-9]*)
      printf 'appa protection: %s/health names no process to stop: %s\n' "$runtime_url" "$1" >&2
      return 1
      ;;
  esac
  # No process at that pid: a concurrent starter already stopped it, and
  # the wait below sees the port refuse or that starter's replacement.
  owner=$(ps -o uid= -p "$1" 2>/dev/null | tr -d ' ')
  if [ -n "$owner" ]; then
    if [ "$owner" != "$(id -u)" ]; then
      printf 'appa protection: pid %s is not this user'"'"'s appa runtime; not stopping it\n' "$1" >&2
      return 1
    fi
    case $(ps -o comm= -p "$1" 2>/dev/null) in
      appa | */appa) ;;
      *)
        printf 'appa protection: pid %s is not appa runtime; not stopping it\n' "$1" >&2
        return 1
        ;;
    esac
    kill "$1" 2>/dev/null || true
  fi
  deadline=$(($(date +%s) + 10))
  while :; do
    case $(probe) in
      '') return 0 ;;
      ok) exit 0 ;;
      "stale $1") ;;
      *)
        printf 'appa protection: %s answers neither ok nor the stale runtime being stopped\n' "$runtime_url" >&2
        return 1
        ;;
    esac
    if [ "$(date +%s)" -ge "$deadline" ]; then
      printf 'appa protection: the stale runtime at %s (pid %s) did not stop\n' "$runtime_url" "$1" >&2
      return 1
    fi
    sleep 1
  done
}

# Without curl every probe reads as an endpoint that never answers, and a
# runtime started under it would be waited on for nothing.
if ! command -v curl >/dev/null 2>&1; then
  printf 'appa protection: curl is not installed, so %s/health cannot be probed\n' "$runtime_url" >&2
  exit 1
fi

if healthy; then
  exit 0
fi
answer=$(probe)
case $answer in
  stale\ *) stop_stale_runtime "${answer#stale }" || exit 1 ;;
esac

if [ ! -x "$APPA_BIN" ]; then
  printf 'appa protection: appa is not installed at %s. Run in a plain terminal: appa init claude-code\n' \
    "$APPA_BIN" >&2
  exit 1
fi

# The runtime writes the default policy on its first start and refuses to
# start when it cannot.
mkdir -p "$(dirname "$APPA_CONFIG")" "$APPA_DATA_DIR"

nohup "$APPA_BIN" runtime --listen "$APPA_LISTEN" \
  --config "$APPA_CONFIG" --db "$APPA_DATA_DIR/appa.db" \
  >>"$APPA_DATA_DIR/runtime.stdout.log" 2>>"$APPA_DATA_DIR/runtime.stderr.log" \
  </dev/null &

# The whole start must finish inside the timeout hooks.json declares for
# SessionStart, so the wait is a wall-clock budget. A count of probes is
# not: one probe costs microseconds where the port refuses and the full
# deadline where it hangs.
deadline=$(($(date +%s) + 20))
while [ "$(date +%s)" -lt "$deadline" ]; do
  if healthy; then
    exit 0
  fi
  sleep 1
done
printf 'appa protection: runtime did not become healthy at %s. Its own error is the last line of %s\n' \
  "$runtime_url" "$APPA_DATA_DIR/runtime.stderr.log" >&2
exit 1
