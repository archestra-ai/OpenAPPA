#!/bin/sh
# Starts the installed appa-runtime when no healthy runtime answers.
# Two callers share it: the last step of the install (the appa-setup skill)
# and every protected SessionStart, which runs it before the hooks post
# their first event. A protected session therefore needs no login service,
# and an install that ends here leaves the runtime up, so the first
# protected session pays nothing for the start. Exit 0 means a healthy
# runtime answers; any other exit makes the chained protection hook block,
# and tells the install that it has nothing to report as running.
# Installing the binary is not this script's job: the appa-setup skill
# does that (skills/appa-setup).
#
# Concurrent starts need no lock. The runtime binds the loopback port, so
# the first process to bind serves and every later one exits at once with
# "address already in use"; both callers then see the same healthy
# runtime. The port is a mutex that cannot go stale, while a lock file
# outlives the hook that Claude Code kills at its timeout and would block
# every start after it.
set -u

runtime_url=${APPA_RUNTIME_URL:-http://127.0.0.1:8787}

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
# so only this user's own appa-runtime process is ever signalled.
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
      printf 'appa protection: pid %s is not this user'"'"'s appa-runtime; not stopping it\n' "$1" >&2
      return 1
    fi
    case $(ps -o comm= -p "$1" 2>/dev/null) in
      appa-runtime | */appa-runtime) ;;
      *)
        printf 'appa protection: pid %s is not appa-runtime; not stopping it\n' "$1" >&2
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

if healthy; then
  exit 0
fi
answer=$(probe)
case $answer in
  stale\ *) stop_stale_runtime "${answer#stale }" || exit 1 ;;
esac

case "$(uname -s)" in
  Darwin)
    config_dir=${APPA_CONFIG_DIR:-"$HOME/Library/Application Support/appa"}
    data_dir=${APPA_DATA_DIR:-"$HOME/Library/Application Support/appa"}
    ;;
  *)
    config_dir=${APPA_CONFIG_DIR:-"${XDG_CONFIG_HOME:-$HOME/.config}/appa"}
    data_dir=${APPA_DATA_DIR:-"${XDG_DATA_HOME:-$HOME/.local/share}/appa"}
    ;;
esac

expected_binary=${APPA_INSTALL_DIR:-"$HOME/.local/bin"}/appa-runtime
binary=$expected_binary
if [ ! -x "$binary" ]; then
  binary=$(command -v appa-runtime 2>/dev/null) || {
    printf 'appa protection: appa-runtime is not installed; expected at %s. Run in a plain terminal: claude /appa-setup\n' \
      "$expected_binary" >&2
    exit 1
  }
fi

# The runtime writes the default policy on its first start and refuses to
# start when it cannot. Both directories must exist first: they are one
# path on macOS and two different paths everywhere else.
mkdir -p "$config_dir" "$data_dir"

nohup "$binary" --config "$config_dir/appa.toml" --db "$data_dir/appa.db" \
  >>"$data_dir/runtime.stdout.log" 2>>"$data_dir/runtime.stderr.log" \
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
  "$runtime_url" "$data_dir/runtime.stderr.log" >&2
exit 1
