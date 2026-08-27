#!/bin/sh
set -eu

printf '%s\n' "$*" >>"$FAKE_CLAUDE_LOG"

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
