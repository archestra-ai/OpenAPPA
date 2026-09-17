# The OpenAPPA Go runtime image: the stock kagent go runtime
# construction, built from source, and AppaPluginKagent while
# `APPA_ENABLED` is true. The image leaves that knob unset, so it
# serves the stock runtime. kagent's controller derives this image's
# name from the python one — the last path segment becomes
# `golang-adk` — so tag that alias beside appa-kagent-adk.
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.
FROM golang:1.26-bookworm@sha256:9fdc884aacc3bec89b20ffc69f4bb369c78210e3e4f600387b5128b12c199f81 AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/appa-kagent-adk-go ./cmd/appa-kagent-adk-go

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /out/appa-kagent-adk-go /usr/local/bin/appa-kagent-adk-go
# The controller sends --host, --port and --filepath; /config is its mount.
ENTRYPOINT ["/usr/local/bin/appa-kagent-adk-go"]
