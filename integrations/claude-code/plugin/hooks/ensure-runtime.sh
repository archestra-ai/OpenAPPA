#!/bin/sh
# Starts the installed appa-runtime-v2 when no healthy runtime answers.
# SessionStart runs this before the hooks post their first event, so a
# protected session works without a login service. Exit 0 means a healthy
# runtime answers; any other exit makes the chained protection hook block.
# Installing the binary is not this script's job: an unprotected session
# offers that as a prompted task (hooks/setup-appa.md).
#
# The lock only serializes concurrent SessionStarts during the poll window:
# whichever session creates it spawns the runtime, the others wait on
# /health. It is removed on every exit path, so it cannot go stale across
# sessions.
set -u

runtime_url=${APPA_RUNTIME_URL:-http://127.0.0.1:8787}

healthy() {
  [ "$(curl -sf -m 2 "$runtime_url/health" 2>/dev/null || true)" = ok ]
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

expected_binary=${APPA_INSTALL_DIR:-"$HOME/.local/bin"}/appa-runtime-v2
binary=$expected_binary
if [ ! -x "$binary" ]; then
  binary=$(command -v appa-runtime-v2 2>/dev/null) || {
    printf 'appa protection: appa-runtime-v2 is not installed; expected at %s. Run in a plain terminal: claude "set up APPA"\n' \
      "$expected_binary" >&2
    exit 1
  }
fi

mkdir -p "$data_dir"
lock_dir=$data_dir/runtime.hook.lock
if mkdir "$lock_dir" 2>/dev/null; then
  trap 'rmdir "$lock_dir" 2>/dev/null || true' 0 1 2 15
  nohup "$binary" --config "$config_dir/appa.toml" --db "$data_dir/appa.db" \
    >>"$data_dir/runtime.stdout.log" 2>>"$data_dir/runtime.stderr.log" \
    </dev/null &
fi

attempts=0
while [ "$attempts" -lt 15 ]; do
  if healthy; then
    exit 0
  fi
  attempts=$((attempts + 1))
  sleep 1
done
printf 'appa protection: runtime did not become healthy at %s\n' "$runtime_url" >&2
exit 1
