#!/bin/sh
# Starts the installed appa-runtime when no healthy runtime answers.
# Two callers share it: the last step of the install (hooks/setup-appa.md)
# and every protected SessionStart, which runs it before the hooks post
# their first event. A protected session therefore needs no login service,
# and an install that ends here leaves the runtime up, so the first
# protected session pays nothing for the start. Exit 0 means a healthy
# runtime answers; any other exit makes the chained protection hook block,
# and tells the install that it has nothing to report as running.
# Installing the binary is not this script's job: an unprotected session
# offers that as a prompted task (hooks/setup-appa.md).
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
healthy() {
  [ "$(curl -sf -m 1 "$runtime_url/health" 2>/dev/null || true)" = ok ]
}

if healthy; then
  exit 0
fi

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
    printf 'appa protection: appa-runtime is not installed; expected at %s. Run in a plain terminal: claude "set up APPA"\n' \
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
