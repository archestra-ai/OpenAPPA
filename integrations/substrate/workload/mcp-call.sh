#!/bin/sh
# One MCP tool call with curl: no agent, no SDK. It shows that the gateway
# checks a request by what it carries, whichever client sent it.
#
#   mcp-call.sh TOOL ARGUMENTS_JSON
set -eu

tool=$1
arguments=$2
url=${GITHUB_MCP_URL:-https://api.githubcopilot.com/mcp/}
auth="Authorization: Bearer ${GITHUB_TOKEN:?}"

session=$(curl -sS -D - -o /dev/null "$url" -H "$auth" \
  -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}' |
  tr -d '\r' | awk 'tolower($1) == "mcp-session-id:" { print $2 }')

curl -sS "$url" -H "$auth" -H "mcp-session-id: $session" \
  -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null

curl -sS "$url" -H "$auth" -H "mcp-session-id: $session" \
  -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$arguments}}"
