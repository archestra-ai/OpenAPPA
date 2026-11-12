FROM golang:1.26.7 AS build
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY *.go log.html ./
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /appa-extproc .

FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /appa-extproc /appa-extproc
ENTRYPOINT ["/appa-extproc"]
