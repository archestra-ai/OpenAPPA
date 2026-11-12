#!/bin/sh
# Start the OpenAPPA runtime on loopback, then the actor's HTTP server.
#
# The runtime starts here, before the first request, so the wakeup probe can
# require it. It is the same command a protected SessionStart runs when no
# runtime answers, so that hook finds this one healthy and posts to it.
#
# Environment:
#   ARCHESTRA_MCP_URL        the Archestra MCP gateway's /v1/mcp/<id> URL
#   ARCHESTRA_TOKEN          a placeholder; the egress gateway injects the token
#   CLAUDE_CODE_OAUTH_TOKEN  a placeholder; the egress gateway injects the token
set -eu

: "${ARCHESTRA_MCP_URL:?ARCHESTRA_MCP_URL is required}"
: "${ARCHESTRA_TOKEN:?ARCHESTRA_TOKEN is required (a placeholder is enough)}"

home=/home/agent
appa=$home/.local/share/appa/bin/appa
config=$home/.config/appa/appa.toml
data=$home/.local/share/appa

cat >"$home/mcp.json" <<EOF
{
  "mcpServers": {
    "appa": { "type": "http", "url": "http://127.0.0.1:8787/mcp" },
    "archestra": {
      "type": "http",
      "url": "$ARCHESTRA_MCP_URL",
      "headers": { "Authorization": "Bearer $ARCHESTRA_TOKEN" }
    }
  }
}
EOF

"$appa" runtime --adapter claude-code --listen 127.0.0.1:8787 \
  --config "$config" --db "$data/appa.db" &

exec node "$home/actor/actor-server.mjs"
