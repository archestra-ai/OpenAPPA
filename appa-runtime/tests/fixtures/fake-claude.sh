#!/bin/sh
# The `claude` CLI as activation and removal drive it: the user-scope MCP
# registration under APPA's name, kept in $FAKE_CLAUDE_HOME/mcp-appa as the
# JSON `add-json` was given. Every invocation is logged, one line each.
#
# FAKE_CLAUDE_RENDERS_TEMPLATE reports a template URL the way Claude does:
# without its default.
#
# FAKE_CLAUDE_FAIL_ONCE=mcp-add fails the first `add-json` of this fixture;
# `mcp-add-always` fails every one, which is what a rollback that cannot put
# the earlier registration back looks like.
set -eu

printf '%s\n' "$*" >>"$FAKE_CLAUDE_LOG"

store="$FAKE_CLAUDE_HOME/mcp-appa"
failure_marker="$FAKE_CLAUDE_HOME/failed-${FAKE_CLAUDE_FAIL_ONCE:-none}"
case "${FAKE_CLAUDE_FAIL_ONCE:-}:$*" in
  mcp-add-always:mcp\ add-json\ *)
    printf 'deliberate persistent fake Claude failure: %s\n' "$*" >&2
    exit 71
    ;;
  mcp-add:mcp\ add-json\ *)
    if [ ! -f "$failure_marker" ]; then
      mkdir -p "$FAKE_CLAUDE_HOME"
      : >"$failure_marker"
      printf 'deliberate fake Claude failure: %s\n' "$*" >&2
      exit 70
    fi
    ;;
esac

case "$*" in
  "mcp get appa")
    if [ ! -f "$store" ]; then
      printf 'No MCP server named "appa". Run `claude mcp add` to add one.\n' >&2
      exit 1
    fi
    url=$(sed 's/.*"url":"\([^"]*\)".*/\1/' "$store")
    if [ -n "${FAKE_CLAUDE_RENDERS_TEMPLATE:-}" ]; then
      case "$url" in
        '${APPA_RUNTIME_URL:-'*'}/mcp') url='${APPA_RUNTIME_URL}/mcp' ;;
      esac
    fi
    printf 'appa:\n  Scope: User config (available in all your projects)\n  Status: ✔ Connected\n  Type: http\n  URL: %s\n\nTo remove this server, run: claude mcp remove appa -s user\n' "$url"
    ;;
  "mcp remove appa --scope user")
    if [ ! -f "$store" ]; then
      printf 'No MCP server named "appa" in user scope\n' >&2
      exit 1
    fi
    rm -f "$store"
    printf 'Removed MCP server appa from user config\n'
    ;;
  mcp\ add-json\ --scope\ user\ appa\ *)
    if [ -f "$store" ]; then
      printf 'MCP server appa already exists in user config\n' >&2
      exit 1
    fi
    mkdir -p "$FAKE_CLAUDE_HOME"
    printf '%s\n' "$6" >"$store"
    printf 'Added http MCP server appa to user config\n'
    ;;
  *)
    printf 'unexpected fake claude invocation: %s\n' "$*" >&2
    exit 64
    ;;
esac
