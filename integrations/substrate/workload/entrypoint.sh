#!/bin/bash
# Configure stock Claude Code and Codex for GitHub's MCP server, then serve a
# terminal (ttyd, port 7681) and a health check for Substrate's wakeup probe
# (port 8080).
#
# Every credential in this environment is a placeholder. The actor's
# EgressPolicy replaces each one outside the sandbox.
#
# Environment:
#   CLAUDE_CODE_OAUTH_TOKEN  placeholder; injected by the egress gateway
#   OPENAI_API_KEY     placeholder; injected by the egress gateway
#   GITHUB_TOKEN       placeholder; injected by the egress gateway
#   GITHUB_MCP_URL     GitHub's MCP endpoint (default https://api.githubcopilot.com/mcp/)
set -euo pipefail

: "${CLAUDE_CODE_OAUTH_TOKEN:?}" "${OPENAI_API_KEY:?}" "${GITHUB_TOKEN:?}"
url=${GITHUB_MCP_URL:-https://api.githubcopilot.com/mcp/}
home=/home/agent

# The terminal's login shell carries the actor's settings.
{
  for name in ACTOR_ATESPACE CLAUDE_CODE_OAUTH_TOKEN OPENAI_API_KEY GITHUB_TOKEN \
    NODE_EXTRA_CA_CERTS SSL_CERT_FILE CURL_CA_BUNDLE CODEX_CA_CERTIFICATE; do
    [[ -n "${!name:-}" ]] && printf 'export %s=%q\n' "$name" "${!name}"
  done
  printf 'export GITHUB_MCP_URL=%q\n' "$url"
  cat <<'PROFILE'
export CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1
export PATH="$HOME/actor:$PATH"
# Substrate projects the actor's own name; every actor of a template shares
# its golden snapshot, so the name cannot come from the environment.
ACTOR_NAME=$(cat /run/ate/actor/name 2>/dev/null || echo actor)
PS1='\[\e[1;32m\]${ACTOR_ATESPACE}/${ACTOR_NAME}\[\e[0m\]:\[\e[1;34m\]\w\[\e[0m\]\$ '
cd "$HOME/work"
PROFILE
} >"$home/.actor-env"
# shellcheck disable=SC2016 # $HOME expands when the shell starts
echo '. "$HOME/.actor-env"' >"$home/.bash_profile"
# shellcheck disable=SC2016
echo '. "$HOME/.actor-env"' >"$home/.bashrc"

# Claude Code: GitHub's MCP server, no first-run prompts, GitHub tools allowed.
cat >"$home/.claude.json" <<JSON
{
  "hasCompletedOnboarding": true,
  "theme": "dark",
  "projects": { "$home/work": { "hasTrustDialogAccepted": true, "allowedTools": [] } },
  "mcpServers": {
    "github": {
      "type": "http",
      "url": "$url",
      "headers": { "Authorization": "Bearer $GITHUB_TOKEN" }
    }
  }
}
JSON
cat >"$home/.claude/settings.json" <<'JSON'
{ "permissions": { "allow": ["mcp__github"] } }
JSON

# Codex: the same MCP server. Substrate's sandbox is the boundary, so Codex
# runs without its own sandbox or approval prompts. The egress gateway does
# not carry WebSockets, so Codex talks to the Responses API over HTTPS.
cat >"$home/.codex/config.toml" <<TOML
approval_policy = "never"
sandbox_mode = "danger-full-access"
model_provider = "openai-https"

[model_providers.openai-https]
name = "OpenAI"
base_url = "https://api.openai.com/v1"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
supports_websockets = false

[projects."$home/work"]
trust_level = "trusted"

[mcp_servers.github]
url = "$url"
bearer_token_env_var = "GITHUB_TOKEN"
TOML
node -e 'require("http").createServer((q, s) => s.end("ok")).listen(8080)' &
exec ttyd --writable --port 7681 \
  -t fontSize=15 -t disableLeaveAlert=true -t 'theme={"background":"#0d1117"}' \
  bash --login
