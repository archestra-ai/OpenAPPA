# The demo actor: Claude Code protected by OpenAPPA, served over HTTP for
# Substrate's router. The image installs OpenAPPA the way a workstation does
# (`install.sh`, then `appa plugin install claude-code`), so the hooks, the
# runtime binary, and `clappa` are the released ones; only the policy is the
# demo's own.
FROM node:22-slim

ARG CLAUDE_CODE_VERSION=2.1.322
# Empty installs the latest release.
ARG APPA_VERSION=

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"

RUN useradd --create-home --uid 10001 agent
USER agent
WORKDIR /home/agent
ENV PATH=/home/agent/.local/bin:$PATH \
    SHELL=/bin/sh

RUN curl -fsSL https://openappa.com/install.sh \
      | APPA_VERSION="${APPA_VERSION}" APPA_VERIFY_PROVENANCE=skip sh \
  && appa plugin install claude-code \
  && mkdir -p /home/agent/work

# The install keeps an existing policy, so the demo's replaces the one it
# wrote afterwards.
COPY --chown=agent:agent demo.appa.toml /home/agent/.config/appa/appa.toml
COPY --chown=agent:agent actor-server.mjs entrypoint.sh /home/agent/actor/
COPY --chown=agent:agent ui /home/agent/actor/ui

# Headless and quiet: no updater, no telemetry, no extra hosts for the
# EgressPolicy to allow.
ENV CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
    DISABLE_AUTOUPDATER=1 \
    DISABLE_TELEMETRY=1 \
    PORT=8080

EXPOSE 8080
ENTRYPOINT ["/home/agent/actor/entrypoint.sh"]
