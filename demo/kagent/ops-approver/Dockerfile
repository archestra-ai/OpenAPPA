# Build context is this directory (the crate is standalone):
#   docker build -t ops-approver:poc demo/kagent/ops-approver
# Pin to bookworm so the runtime glibc (debian:bookworm-slim) matches the
# build glibc — rust:1-slim tracks trixie and links a newer glibc.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --bin ops-approver

FROM debian:bookworm-slim
COPY --from=build /src/target/release/ops-approver /usr/local/bin/ops-approver
ENTRYPOINT ["ops-approver"]
