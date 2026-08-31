#!/bin/sh
set -eu

printf '%s\n' "$*" >>"$FAKE_CLAUDE_LOG"

failure_marker="$FAKE_CLAUDE_HOME/failed-${FAKE_CLAUDE_FAIL_ONCE:-none}"
case "${FAKE_CLAUDE_FAIL_ONCE:-}:$*" in
  plugin-install-always:"plugin install appa-runtime@appa --scope user")
    printf 'deliberate persistent fake Claude failure: %s\n' "$*" >&2
    exit 71
    ;;
  marketplace-add:plugin\ marketplace\ add\ * | plugin-install:"plugin install appa-runtime@appa --scope user")
    if [ ! -f "$failure_marker" ]; then
      mkdir -p "$FAKE_CLAUDE_HOME"
      : >"$failure_marker"
      printf 'deliberate fake Claude failure: %s\n' "$*" >&2
      exit 70
    fi
    ;;
esac

case "$*" in
  "plugin marketplace list")
    printf 'Configured marketplaces:\n'
    if [ -f "$FAKE_CLAUDE_HOME/marketplace-appa" ]; then
      printf '\n  ❯ appa\n    Source: local\n'
    fi
    ;;
  "plugin marketplace remove appa")
    rm -f "$FAKE_CLAUDE_HOME/marketplace-appa"
    ;;
  plugin\ marketplace\ add\ *)
    mkdir -p "$FAKE_CLAUDE_HOME"
    : >"$FAKE_CLAUDE_HOME/marketplace-appa"
    ;;
  "plugin uninstall appa-runtime@appa --scope user --yes")
    mkdir -p "$FAKE_CLAUDE_HOME/plugins"
    printf '{"version":2,"plugins":{}}\n' >"$FAKE_CLAUDE_HOME/plugins/installed_plugins.json"
    ;;
  "plugin install appa-runtime@appa --scope user")
    mkdir -p "$FAKE_CLAUDE_HOME/plugins"
    printf '{"version":2,"plugins":{"appa-runtime@appa":[{"scope":"user","installPath":"%s","version":"test"}]}}\n' \
      "$FAKE_PLUGIN_ROOT" >"$FAKE_CLAUDE_HOME/plugins/installed_plugins.json"
    ;;
  *)
    printf 'unexpected fake claude invocation: %s\n' "$*" >&2
    exit 64
    ;;
esac
