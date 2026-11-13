#!/bin/sh
# Point Claude Code at GitHub's MCP server, then serve the actor's front door.
#
# Environment:
#   GITHUB_MCP_URL  GitHub's MCP endpoint (default https://api.githubcopilot.com/mcp/)
#   GITHUB_TOKEN    a placeholder; the egress gateway injects the token
set -eu

: "${GITHUB_TOKEN:?GITHUB_TOKEN is required (a placeholder is enough)}"
url=${GITHUB_MCP_URL:-https://api.githubcopilot.com/mcp/}

cat >/home/agent/mcp.json <<JSON
{
  "mcpServers": {
    "github": {
      "type": "http",
      "url": "$url",
      "headers": { "Authorization": "Bearer $GITHUB_TOKEN" }
    }
  }
}
JSON

exec node /home/agent/actor/actor-server.mjs
