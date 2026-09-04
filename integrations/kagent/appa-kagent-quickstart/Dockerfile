# appa-kagent-quickstart: one pod, nothing else to deploy.
#
# The image bundles the python runtime and `appa-runtime` itself. It
# ships with the gate off, so it serves what the stock kagent runtime
# serves until an operator sets `APPA_ENABLED=true`.
#
# With the gate on, the entrypoint starts `appa runtime` on
# 127.0.0.1:8787 with the packaged example policy — or the policy the
# deployment mounts over `APPA_CONFIG` — points `APPA_RUNTIME_URL` at
# it, and execs the gated runtime under the stock args, port, and
# readiness contract. The bundled runtime serves `/mcp` too, so remedy
# plans execute the same way as against a deployed runtime.
#
#   docker build -f integrations/kagent/appa-kagent-quickstart/Dockerfile -t appa-kagent-quickstart .
#   (build context: the repository root — the builder stage compiles appa)
#
# Two routes turn the gate on, because kagent's controller injects no
# global env:
#
#   1. Per agent, through `spec.declarative.deployment.env`:
#        env:
#          - name: APPA_ENABLED
#            value: "true"
#   2. Per cluster, through a derived image that `controller.agentImage`
#      points at:
#        FROM <this image>
#        ENV APPA_ENABLED=true
#
# Configuration:
#   APPA_ENABLED  `true` runs the gate. Unset, empty or `false` runs the
#                 stock kagent runtime and starts no bundled runtime.
#                 Any other value refuses the start. The image leaves it
#                 unset, so the gate is off by default.
#   APPA_CONFIG   the appa.toml the bundled runtime loads
#                 (default: the packaged example policy)
#   APPA_CONFIG_CONTENTS
#                 complete TOML written to the pod-local appa.toml before
#                 the bundled runtime starts; intended for Agent env config
#   APPA_DATA_DIR writable directory for that generated file
#                 (default: /.kagent)
#   APPA_DB       the trajectory log path (default: pod-local, dies with
#                 the pod — the quickstart's documented limit)
#
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.

ARG KAGENT_APP_IMAGE=cr.kagent.dev/kagent-dev/kagent/app:0.9.12@sha256:5ee30b4584e8de3266eb3cc11f5c46e8627716339d04d14166c50bda5f0f4182

FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS builder
WORKDIR /build
# The path deps inherit from the root workspace, so the workspace
# manifest and every member must be present to build any of them.
COPY Cargo.toml Cargo.lock ./
COPY appa-example-agent appa-example-agent
COPY appa-agent-python appa-agent-python
COPY appa-engine appa-engine
COPY appa-policy appa-policy
COPY appa-runtime appa-runtime
COPY appa-runtime-api appa-runtime-api
COPY appa-adapter-claude-code appa-adapter-claude-code
COPY appa-adapter-kagent appa-adapter-kagent
COPY appa-builtin appa-builtin
COPY appa-eventlog appa-eventlog
COPY bench bench
COPY website-chat-playground website-chat-playground
# appa's build.rs stages the Claude Code plugin source into the binary's
# build identity, so a release build needs these paths in-context.
COPY integrations integrations
COPY batteries batteries
COPY website/content/docs/contracts.md website/content/docs/contracts.md
RUN cargo build --release --locked --package appa

FROM python:3.13-slim@sha256:9d2e5553305c7c7b0097999bb17187c69b921ccd6bc9d40e4bb5ebe652c00285 AS wheel
COPY --from=ghcr.io/astral-sh/uv:0.11.15@sha256:e590846f4776907b254ac0f44b5b380347af5d90d668138ca7938d1b0c2f98d3 /uv /usr/local/bin/uv
COPY integrations/kagent/appa-kagent-adk /pkg
RUN uv build --wheel /pkg --out-dir /dist

FROM ${KAGENT_APP_IMAGE}

COPY --from=builder /build/target/release/appa /usr/local/bin/appa
COPY --from=wheel --chown=python:pythongroup /dist/ /tmp/appa-dist/
COPY --chown=python:pythongroup integrations/kagent/examples/kagent.appa.toml /opt/appa/kagent.appa.toml
COPY --chown=python:pythongroup batteries /opt/appa/batteries
COPY --chown=python:pythongroup scripts/appa-refresh-batteries.py /usr/local/bin/appa-refresh-batteries
COPY --chown=python:pythongroup integrations/kagent/appa-kagent-quickstart/entrypoint.sh /opt/appa/entrypoint.sh

RUN uv pip install --python /.kagent/.venv/bin/python --no-deps --offline /tmp/appa-dist/*.whl \
    && chmod 0755 /usr/local/bin/appa-refresh-batteries /opt/appa/entrypoint.sh

ENV APPA_CONFIG=/opt/appa/kagent.appa.toml
ENV APPA_DB=/.kagent/appa.db
ENV APPA_BATTERIES_DIR=/opt/appa/batteries
ENV APPA_DATA_DIR=/.kagent

ENTRYPOINT ["/opt/appa/entrypoint.sh"]
