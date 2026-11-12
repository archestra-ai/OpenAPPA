# OpenAPPA for Agent Substrate

This directory holds the OpenAPPA integration for
[Agent Substrate](https://github.com/agent-substrate/substrate). It
includes the following:

- An `ext_proc` processor that enforces OpenAPPA at Substrate's egress gateway.
- The manifests that run the processor with an OpenAPPA runtime.
- A stock agent image to try it with.

- **Public documentation**: [website/content/docs/substrate.md](../../website/content/docs/substrate.md)

This project is not affiliated with Agent Substrate. Trademarks belong to
their respective owners.

## Components

```text
integrations/substrate/
├── appa-extproc/                  # Go ext_proc processor (gRPC, mTLS with Substrate pod certificates)
│   ├── processor.go               # per-request stream: gates tools/call, reports results
│   ├── runtime.go                 # OpenAPPA hook wire and remedy plans
│   ├── jsonrpc.go                 # MCP JSON-RPC over JSON and SSE
│   ├── admin.go, log.html         # decision log, status and reset (loopback)
│   └── testdata/flow.appa.toml    # policy for the runtime-backed test
├── policy/appa.toml               # the deployed policy: the GitHub battery
├── manifests/
│   ├── enforcement.yaml.tmpl      # appa-system: processor + runtime pod, Service
│   ├── workerpool.yaml.tmpl       # the agent's namespace and WorkerPool
│   ├── actor-template.yaml.tmpl   # stock Claude Code actor, placeholder credentials
│   └── egress-policy.yaml.tmpl    # api.anthropic.com and api.githubcopilot.com, tokens injected
├── workload/                      # the agent image: no OpenAPPA inside
│   ├── actor-server.mjs           # console, POST /run (Claude Code), POST /curl
│   ├── mcp-call.sh                # one MCP tool call with curl
│   └── ui/index.html
└── deploy.sh                      # build, push, and deploy to a kind install
```

## Test

```sh
cd integrations/substrate/appa-extproc
go test ./...
```

`TestFlowAgainstRuntime` runs against a served runtime when
`APPA_TEST_RUNTIME_URL` is set:

```sh
appa runtime --adapter kagent --listen 127.0.0.1:18788 \
  --config testdata/flow.appa.toml --db "$(mktemp -d)/appa.db" &
APPA_TEST_RUNTIME_URL=http://127.0.0.1:18788 go test ./...
```
