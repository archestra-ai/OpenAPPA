#!/bin/sh
# The APPA statusline: the pixel mascot, plus the root trajectory's
# current Trust and Audience from the runtime's /status read, plus
# policy statistics from the deployment's appa.toml.
#
# The mark is the website mascot as half blocks — solid pixels, eyes
# as terminal-background gaps, like the SVG. The chips render the
# statusline's stdin `session_id` mapped to the trajectory the Claude
# Code adapter names, `cc:<session_id>`.
#
# The second row counts the policy's tools: `tools:` is every
# [[policy.tool]] entry, `rules:` the entries carrying label rules —
# more than a bare `name`, an empty `delta = {}`, or a `parameters`
# argument pin, which constrains a call's arguments and labels
# nothing. The hint names the skill that adds rules. The policy is the file the runtime's
# status read names in `policy_path` — the served `--config` — so the
# counts follow the runtime the session gates through; when the read
# gives no path, the platform default under APPA_CONFIG_DIR's rules is
# the fallback.
#
# A statusline fails open, the opposite of the hooks' `|| exit 2`:
# every failure — runtime down, unknown trajectory, missing jq or
# curl, malformed stdin, unreadable policy — prints the mascot alone
# and exits 0.
#
# An unprotected session (no APPA_GATE=1) never queries the runtime; it
# prints the mascot with a reminder that clappa starts a protected
# session.
input=$(cat)
if [ "${APPA_GATE:-}" != 1 ]; then
  printf '▄█▄▄▄█▄  unprotected — run clappa to protect\n██▄█▄██\n'
  exit 0
fi
chips=''
live_policy=''
if command -v jq >/dev/null 2>&1 && command -v curl >/dev/null 2>&1; then
  sid=$(printf '%s' "$input" | jq -er '.session_id' 2>/dev/null) &&
    body=$(curl -sf --connect-timeout 0.1 -m 0.3 --get \
      --data-urlencode "trajectory=cc:${sid}" \
      "${APPA_RUNTIME_URL:-http://127.0.0.1:8787}/status" 2>/dev/null) &&
    chips=$(printf '%s' "$body" |
      jq -er 'select((.trust | type) == "string" and (.audience | type) == "string")
        | "trust:\(.trust)  audience:\(.audience)"' 2>/dev/null) ||
    chips=''
  live_policy=$(printf '%s' "${body:-}" |
    jq -er 'select((.policy_path | type) == "string") | .policy_path' 2>/dev/null) ||
    live_policy=''
fi

if [ -n "$live_policy" ] && [ -f "$live_policy" ]; then
  policy=$live_policy
else
  case "$(uname -s)" in
    Darwin) config_dir=${APPA_CONFIG_DIR:-"$HOME/Library/Application Support/appa"} ;;
    *) config_dir=${APPA_CONFIG_DIR:-"${XDG_CONFIG_HOME:-$HOME/.config}/appa"} ;;
  esac
  policy=$config_dir/appa.toml
fi
stats=''
if [ -f "$policy" ] && command -v awk >/dev/null 2>&1; then
  stats=$(awk '
    /^\[\[policy\.tool\]\]/ { if (intool && tuned) rules++; total++; intool=1; tuned=0; next }
    /^\[policy\.tool\./ { if (intool) tuned=1; next }
    /^\[/ { if (intool && tuned) rules++; intool=0; next }
    intool && /^[A-Za-z_]+[ \t]*=/ {
      key=$1
      if (key == "name" || key == "parameters") next
      if (key == "delta") {
        line=$0; sub(/^[^=]*=[ \t]*/, "", line)
        if (line ~ /^\{[ \t]*\}[ \t]*$/) next
      }
      tuned=1
    }
    END {
      if (intool && tuned) rules++
      if (total > 0) printf "tools:%d rules:%d", total, rules
    }
  ' "$policy" 2>/dev/null) || stats=''
fi

top='▄█▄▄▄█▄'
bottom='██▄█▄██'
[ -z "$chips" ] || top="$top  $chips"
[ -z "$stats" ] || bottom="$bottom  $stats · /appa-tool-sync adds rules"
printf '%s\n%s\n' "$top" "$bottom"
exit 0
