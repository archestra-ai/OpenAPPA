# Development copy. `appa init claude-code` overwrites this file in the
# deployment it materializes, with the absolute paths that init resolved and no
# environment fallback at all.
#
# This copy exists so `claude --plugin-dir integrations/claude-code/plugin` and
# live-gate-check.py keep working against a checkout, where nothing has been
# rendered yet.
: "${APPA_BIN:=appa}"
: "${APPA_ENDPOINT:=http://127.0.0.1:8787}"
: "${APPA_LISTEN:=127.0.0.1:8787}"

case "$(uname -s)" in
  Darwin)
    : "${APPA_CONFIG:=$HOME/Library/Application Support/appa/appa.toml}"
    : "${APPA_DATA_DIR:=$HOME/Library/Application Support/appa}"
    ;;
  *)
    : "${APPA_CONFIG:=${XDG_CONFIG_HOME:-$HOME/.config}/appa/appa.toml}"
    : "${APPA_DATA_DIR:=${XDG_DATA_HOME:-$HOME/.local/share}/appa}"
    ;;
esac

export APPA_BIN APPA_CONFIG APPA_DATA_DIR APPA_ENDPOINT APPA_LISTEN
