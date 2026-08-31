# Development copy. `appa init claude-code` overwrites this file in the
# deployment it materializes, with the absolute paths that init resolved and no
# environment fallback at all.
#
# This copy exists so `claude --plugin-dir integrations/claude-code/plugin` and
# live-gate-check.py keep working against a checkout, where nothing has been
# rendered yet.
# The same precedence the PowerShell copy applies: an explicit value, then the
# directory override init itself honours, then the platform default.
if [ -z "${APPA_BIN:-}" ]; then
  if [ -n "${APPA_INSTALL_DIR:-}" ]; then
    APPA_BIN="$APPA_INSTALL_DIR/appa"
  else
    APPA_BIN=appa
  fi
fi
: "${APPA_ENDPOINT:=http://127.0.0.1:8787}"
: "${APPA_LISTEN:=127.0.0.1:8787}"

case "$(uname -s)" in
  Darwin)
    : "${APPA_CONFIG_DIR:=$HOME/Library/Application Support/appa}"
    : "${APPA_DATA_DIR:=$HOME/Library/Application Support/appa}"
    ;;
  *)
    : "${APPA_CONFIG_DIR:=${XDG_CONFIG_HOME:-$HOME/.config}/appa}"
    : "${APPA_DATA_DIR:=${XDG_DATA_HOME:-$HOME/.local/share}/appa}"
    ;;
esac
: "${APPA_CONFIG:=$APPA_CONFIG_DIR/appa.toml}"

export APPA_BIN APPA_CONFIG APPA_DATA_DIR APPA_ENDPOINT APPA_LISTEN
