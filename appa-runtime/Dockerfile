# appa-runtime: the shared policy process for a cluster.
#
# The image carries the `appa` binary and the batteries of this release
# at `/opt/appa/batteries`. A deployment mounts policy at
# `APPA_CONFIG` and the trajectory log at `APPA_DB`. The shared runtime
# listens on every interface so Docker and Kubernetes can reach it directly.
#
#   docker build -f appa-runtime/Dockerfile -t appa-runtime .
#   docker run -e APPA_MCP_ALLOWED_HOST=localhost:18787,127.0.0.1:18787 -p 18787:18787 appa-runtime
#   (build context: the repository root)
#
# Each FROM pins its base image by digest. The tag beside the digest names the version.

FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS builder
WORKDIR /build
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
# appa's build.rs stages these two plugin trees into the binary's build
# identity. Kagent harness and chart changes must not invalidate this compile.
COPY integrations/claude-code integrations/claude-code
COPY integrations/appa-guide integrations/appa-guide
COPY batteries batteries
COPY website/content/docs/contracts.md website/content/docs/contracts.md
# The salt `appa yell` signs reports with. Compiled in by client.rs, so the
# build needs the file even though nothing in the image reads it.
COPY receiver/appa-yell/salt.txt receiver/appa-yell/salt.txt
RUN cargo build --release --locked --package appa

FROM python:3.13-slim@sha256:9d2e5553305c7c7b0097999bb17187c69b921ccd6bc9d40e4bb5ebe652c00285
RUN groupadd --system --gid 65532 appa \
    && useradd --system --uid 65532 --gid 65532 --home-dir /var/lib/appa --create-home appa \
    && mkdir -p /opt/appa /etc/appa \
    && chown appa:appa /var/lib/appa /etc/appa

COPY --from=builder --chown=appa:appa /build/target/release/appa /usr/local/bin/appa
COPY --chown=appa:appa batteries /opt/appa/batteries
COPY --chown=appa:appa scripts/appa-refresh-batteries.py /usr/local/bin/appa-refresh-batteries
COPY --chown=appa:appa scripts/appa-guide-runtime.py /opt/appa/appa-guide-runtime
RUN chmod 0755 /usr/local/bin/appa-refresh-batteries /opt/appa/appa-guide-runtime \
    && for operation in inspect reload refresh-check refresh-stage refresh-commit refresh-rollback command-annotator; do \
         ln -s /opt/appa/appa-guide-runtime "/usr/local/bin/appa-guide-${operation}"; \
       done

USER 65532:65532
ENV APPA_CONFIG=/etc/appa/appa.toml
ENV APPA_DB=/var/lib/appa/appa.db
ENV APPA_BATTERIES_DIR=/opt/appa/batteries
ENV APPA_GUIDE_RUNTIME_URL=http://127.0.0.1:18787
WORKDIR /var/lib/appa
EXPOSE 18787
ENTRYPOINT ["/usr/local/bin/appa"]
CMD ["runtime", "--adapter", "kagent", "--listen", "0.0.0.0:18787"]
