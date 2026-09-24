# appa-demo: the chat-playground service behind openappa.com.
#
# The demo runs a pinned runtime independently of the current workspace.
# The patch changes only the model allowlist. Keep its policy and dependencies pinned.
#
#   docker build -t appa-demo .
#   docker run -p 8787:8787 -e APPA_DEMO_OPENROUTER_API_KEY=sk-or-… appa-demo

FROM rust:1.96-bookworm AS builder
WORKDIR /build
# Public source counterpart of the August 13 demo deployment.
ADD https://github.com/archestra-ai/OpenAPPA.git#1742ed27f4b075f9b2eb8864d965cc70a3460fb1 /build
COPY website-chat-playground/terra.patch /tmp/appa-demo-terra.patch
RUN git apply /tmp/appa-demo-terra.patch \
    && cargo build --release --locked --manifest-path demo/appa-demo/Cargo.toml

FROM debian:bookworm-slim
# The pinned runtime uses bundled TLS roots.
RUN useradd --system --create-home appa
USER appa
WORKDIR /home/appa
COPY --from=builder /build/demo/appa-demo/target/release/appa-demo /usr/local/bin/appa-demo
COPY --from=builder --chown=appa /build/demo/appa-demo/world world
ENV APPA_DEMO_WORLD=/home/appa/world
EXPOSE 8787
# CORS origins and the OpenRouter key arrive from the deployment, not the image.
ENTRYPOINT ["appa-demo", "--listen", "0.0.0.0:8787"]
