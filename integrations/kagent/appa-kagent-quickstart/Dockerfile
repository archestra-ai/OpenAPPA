# appa-kagent-quickstart: one pod, nothing else to deploy.
#
# The image bundles the gated python runtime and `appa-runtime` itself.
# The entrypoint starts `appa runtime` on 127.0.0.1:8787 with the
# packaged example policy — or the policy the deployment mounts over
# `APPA_CONFIG` — points `APPA_RUNTIME_URL` at it, and execs the gated
# runtime under the stock args, port, and readiness contract. The
# bundled runtime serves `/mcp` too, so remedy plans execute the same
# way as against a deployed runtime.
#
#   docker build -f integrations/kagent/appa-kagent-quickstart/Dockerfile -t appa-kagent-quickstart .
#   (build context: the repository root — the builder stage compiles appa)
#
# Configuration:
#   APPA_CONFIG  the appa.toml the bundled runtime loads
#                (default: the packaged example policy)
#   APPA_DB      the trajectory log path (default: pod-local, dies with
#                the pod — the quickstart's documented limit)

ARG KAGENT_APP_IMAGE=cr.kagent.dev/kagent-dev/kagent/app:0.9.12

FROM rust:1.96-bookworm AS builder
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

FROM python:3.13-slim AS wheel
COPY --from=ghcr.io/astral-sh/uv:0.11.15 /uv /usr/local/bin/uv
COPY integrations/kagent/appa-kagent-adk /pkg
RUN uv build --wheel /pkg --out-dir /dist

FROM ${KAGENT_APP_IMAGE}

COPY --from=builder /build/target/release/appa /usr/local/bin/appa
COPY --from=wheel --chown=python:pythongroup /dist/ /tmp/appa-dist/
COPY --chown=python:pythongroup integrations/kagent/examples/kagent.appa.toml /opt/appa/kagent.appa.toml
COPY --chown=python:pythongroup integrations/kagent/appa-kagent-quickstart/entrypoint.sh /opt/appa/entrypoint.sh

RUN uv pip install --python /.kagent/.venv/bin/python --no-deps --offline /tmp/appa-dist/*.whl \
    && chmod +x /opt/appa/entrypoint.sh

ENV APPA_CONFIG=/opt/appa/kagent.appa.toml
ENV APPA_DB=/.kagent/appa.db

ENTRYPOINT ["/opt/appa/entrypoint.sh"]
