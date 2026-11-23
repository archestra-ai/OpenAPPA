# The OpenAPPA Go runtime image: the stock kagent go runtime
# construction, built from source, and AppaPluginKagent while
# `APPA_ENABLED` is true. The image leaves that knob unset, so it
# serves the stock runtime. kagent's controller derives this image's
# name from the python one — the last path segment becomes
# `golang-adk` — so tag that alias beside appa-kagent-adk.
# Each FROM and COPY --from pins its base image by digest. The tag beside the digest names the version.
FROM golang:1.26-bookworm@sha256:dc40a13b61304647ed7e5d581d246b86413dfc29d63c6d018310feb93cd98fc0 AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/appa-kagent-adk-go ./cmd/appa-kagent-adk-go

FROM dhi.io/static:20261020-alpine3.23@sha256:c262b6bf548a9adeb9fdc24fc7b845cd52010b8a47d3515abb5dddb4d172184b
WORKDIR /home/nonroot
COPY --from=build /out/appa-kagent-adk-go /usr/local/bin/appa-kagent-adk-go
# The controller sends --host, --port and --filepath; /config is its mount.
ENTRYPOINT ["/usr/local/bin/appa-kagent-adk-go"]
