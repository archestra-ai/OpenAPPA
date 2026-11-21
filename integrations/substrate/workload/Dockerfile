# An ordinary agent image: stock Claude Code and stock Codex in a terminal
# that Substrate's router serves over WebSocket. Nothing in it knows about
# OpenAPPA; the egress gateway enforces the policy on the requests the actor
# makes.
# ttyd serves the terminal; the project's image carries a static build.
FROM tsl0922/ttyd:1.7.7@sha256:9a058c087bca6df456cb728514eafe778ea384369a622b3604c8b5256b67237d AS ttyd

FROM node:22-slim@sha256:e9f565d32859f0cbfb52125eafe8be39390f24dac302dc3cdf566e8ba3ab21ee

ARG CLAUDE_CODE_VERSION=2.1.325
ARG CODEX_VERSION=0.166.0

RUN apt-get update \
  && apt-get install -y --no-install-recommends bash ca-certificates curl git less procps \
  && rm -rf /var/lib/apt/lists/*

COPY --from=ttyd /usr/bin/ttyd /usr/local/bin/ttyd

RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}" "@openai/codex@${CODEX_VERSION}"

RUN useradd --create-home --shell /bin/bash --uid 10001 agent
USER agent
WORKDIR /home/agent
RUN mkdir -p /home/agent/work /home/agent/.claude /home/agent/.codex

COPY --chown=agent:agent entrypoint.sh mcp-call.sh /home/agent/actor/

EXPOSE 7681 8080
ENTRYPOINT ["/home/agent/actor/entrypoint.sh"]
