# appa-demo: the chat-playground service behind openappa.com.
#
# Built from the repository root because `demo/appa-demo` path-depends on the
# sibling crates (`appa-runtime`, `appa-engine`, `appa-agent`). The crate is
# deliberately outside the root workspace and carries its own Cargo.lock.
#
#   docker build -t appa-demo .
#   docker run -p 8787:8787 -e APPA_DEMO_OPENROUTER_API_KEY=sk-or-… appa-demo

FROM rust:1.96-bookworm AS builder
WORKDIR /build
# The path deps inherit from the root workspace (`edition.workspace = true`),
# so the workspace manifest and every member must be present to build any of
# them — cargo refuses a workspace with missing members.
COPY Cargo.toml Cargo.lock ./
COPY appa-agent appa-agent
COPY appa-agent-python appa-agent-python
COPY appa-dojo-sidecar appa-dojo-sidecar
COPY appa-engine appa-engine
COPY appa-gateway appa-gateway
COPY appa-runtime appa-runtime
COPY appa-runtime-v2 appa-runtime-v2
COPY appa-sdk appa-sdk
COPY demo/appa-demo demo/appa-demo
RUN cargo build --release --locked --manifest-path demo/appa-demo/Cargo.toml

FROM debian:bookworm-slim
# TLS roots are compiled in (webpki-roots); the runtime needs only the binary,
# the seed world, and somewhere writable for per-session worlds.
RUN useradd --system --create-home appa
USER appa
WORKDIR /home/appa
COPY --from=builder /build/demo/appa-demo/target/release/appa-demo /usr/local/bin/appa-demo
COPY --chown=appa demo/appa-demo/world world
ENV APPA_DEMO_WORLD=/home/appa/world
EXPOSE 8787
# CORS origins and the OpenRouter key arrive from the deployment, not the image.
ENTRYPOINT ["appa-demo", "--listen", "0.0.0.0:8787"]
