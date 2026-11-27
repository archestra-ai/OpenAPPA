FROM golang:1.26.7@sha256:e30143be198ab04cf7ba25fba83ab3a692ca584c994aad0bf131fa0eb32dd8c1 AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY *.go log.html ./
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /appa-extproc .

FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /appa-extproc /appa-extproc
ENTRYPOINT ["/appa-extproc"]
