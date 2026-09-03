# OpenAPPA for kagent

This directory contains the OpenAPPA integration for [kagent](https://github.com/kagent-dev/kagent), the Kubernetes-native AI agent orchestrator.

OpenAPPA gates every declarative kagent agent on Kubernetes through one install setting: the runtime image. The integration requires no modifications to agent definitions, no forks of kagent, and no forks of the Google Agent Development Kit (ADK).

- **Operator guide**: [website/content/docs/kagent.md](../../website/content/docs/kagent.md)
- **Implementation specification**: [IMPLEMENTATION.md](IMPLEMENTATION.md)
- **Demo Helm chart**: [demo/chart/README.md](demo/chart/README.md)

## Components

```text
integrations/kagent/
├── appa-kagent-adk/         # Python ADK plugin and entrypoint (appa_kagent_adk)
├── appa-kagent-adk-go/      # Go ADK plugin and replacement runtime main
├── appa-kagent-quickstart/  # Unified container image bundling appa-runtime and runtimes
├── demo/                    # Demo Helm chart, mock services, and demo tools
│   ├── chart/               # Helm chart (appa-kagent-demo)
│   ├── mocks/               # Mock external authorities (change-board, annotator)
│   └── demo_tools.py        # Demo toolset MCP server
├── e2e/                     # End-to-end integration test suites
│   ├── a2a/                 # Matrix tests over the A2A protocol
│   └── ui/                  # Browser matrix tests driving the kagent dashboard
├── examples/                # Reference policies (e.g. kagent.appa.toml)
├── fixtures/                # Canonical wire event fixtures shared across languages
└── IMPLEMENTATION.md        # Technical architecture, callback mappings, and specs
```

### 1. Python Runtime (`appa-kagent-adk/`)
Wraps kagent's published Python runtime container image. It ships `AppaPluginKagent`, a Google ADK `BasePlugin` that maps ADK lifecycle callbacks to OpenAPPA `/hook` events, and a replacement entrypoint that preserves the stock arguments while appending the plugin and the `execute_remedy_plan` MCP tool.

### 2. Go Runtime (`appa-kagent-adk-go/`)
Implements `AppaPluginKagent` for Google Go ADK v2. It provides a replacement runtime main that registers the plugin, manages session lineage headers across agent-to-agent delegation, and coordinates human-in-the-loop approvals.

### 3. Quickstart Image (`appa-kagent-quickstart/`)
A self-contained container image bundling both Python and Go runtimes together with an embedded `appa-runtime` binary. When deployed without an external `APPA_RUNTIME_URL`, the image starts `appa-runtime` on `127.0.0.1:8787` using a packaged default policy. When `APPA_RUNTIME_URL` is supplied, it connects to the shared runtime service.

### 4. Codec Crate (`appa-adapter-kagent`)
The Rust codec crate located at `crates/appa-adapter-kagent` (in the workspace root). It is compiled directly into `appa-runtime` and parses wire events sent by `AppaPluginKagent`.

## Quickstart

### Deploy kagent with OpenAPPA

Install kagent CRDs and the kagent controller, setting `controller.agentImage` to the OpenAPPA quickstart image:

```sh
# 1. Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace

# 2. Install kagent controller with OpenAPPA image
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.7.0 --wait
```

### Deploy the Interactive Demo

Deploy the demo chart with your OpenAI API key:

```sh
helm upgrade --install appa-kagent-demo ./demo/chart \
  -n kagent --set openai.apiKey="$OPENAI_API_KEY" --wait
```

Port-forward the kagent dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:80
```

Open [http://localhost:8901](http://localhost:8901) to explore 16 pre-seeded demonstration chats showcasing data exfiltration blocks, untrusted ingress quarantine, human-in-the-loop approvals, and multi-agent delegation.

## Building from Source

To build and test the integration images locally (for example, on a local `kind` cluster):

```sh
# Build images
docker build -f appa-kagent-quickstart/Dockerfile -t appa-kagent-quickstart:dev ../../
docker build -t appa-kagent-adk-go:dev appa-kagent-adk-go
docker build -t appa-demo-tools:dev demo
docker build -t appa-demo-mocks:dev demo/mocks

# Load images into kind
kind load docker-image \
  appa-kagent-quickstart:dev \
  appa-kagent-adk-go:dev \
  appa-demo-tools:dev \
  appa-demo-mocks:dev \
  --name <cluster-name>
```

## Running Tests

### Python Unit Tests
The Python tests verify callback conversions, config validation, and wire encoding across both supported Google ADK releases (1.31.1 and 2.8.0):

```sh
cd appa-kagent-adk
uv run pytest
uv run --with google-adk==1.31.1 pytest
```

### Go Unit Tests
The Go tests verify ADK v2 callbacks and wire format parity:

```sh
cd appa-kagent-adk-go
go test ./...
```

### End-to-End Verification Matrices
The test matrices run 17 distinct scenarios against the live cluster:

```sh
# Run A2A protocol test matrix
pytest integrations/kagent/e2e/a2a

# Run UI dashboard test matrix (requires Playwright / Chromium)
pytest integrations/kagent/e2e/ui
```
