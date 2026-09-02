# The OpenAPPA Go runtime image: the stock kagent go runtime construction
# with AppaPluginKagent, built from source. kagent's controller derives
# this image's name from the python one — the last path segment becomes
# `golang-adk` — so tag it beside appa-kagent-quickstart.
FROM golang:1.26-bookworm AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/appa-kagent-adk-go ./cmd/appa-kagent-adk-go

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/appa-kagent-adk-go /usr/local/bin/appa-kagent-adk-go
# The controller sends --host, --port and --filepath; /config is its mount.
ENTRYPOINT ["/usr/local/bin/appa-kagent-adk-go"]
