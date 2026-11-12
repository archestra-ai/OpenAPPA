# An ordinary agent image: stock Claude Code behind a small HTTP front door
# for Substrate's router. Nothing in it knows about OpenAPPA; the egress
# gateway enforces the policy on the requests the actor makes.
FROM node:22-slim@sha256:e9f565d32859f0cbfb52125eafe8be39390f24dac302dc3cdf566e8ba3ab21ee

ARG CLAUDE_CODE_VERSION=2.1.322

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"

RUN useradd --create-home --uid 10001 agent
USER agent
WORKDIR /home/agent
RUN mkdir -p /home/agent/work

COPY --chown=agent:agent actor-server.mjs entrypoint.sh mcp-call.sh /home/agent/actor/
COPY --chown=agent:agent ui /home/agent/actor/ui

# Headless and quiet: no updater, no telemetry, no extra hosts for the
# EgressPolicy to allow.
ENV CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
    DISABLE_AUTOUPDATER=1 \
    DISABLE_TELEMETRY=1 \
    PORT=8080

EXPOSE 8080
ENTRYPOINT ["/home/agent/actor/entrypoint.sh"]
