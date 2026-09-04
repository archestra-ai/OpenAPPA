# OpenAPPA for kagent

This directory contains the OpenAPPA integration for [kagent](https://github.com/kagent-dev/kagent), the Kubernetes-native AI agent orchestrator.

OpenAPPA makes every declarative kagent agent on Kubernetes ready to gate through one install setting: the runtime image (`controller.agentImage`). Each Agent then turns the gate on itself with `APPA_ENABLED=true` in its own `spec.declarative.deployment.env`. Without that value the image serves the agent exactly as the stock kagent image does and gates nothing. Neither step needs a fork of kagent or a fork of the Google Agent Development Kit (ADK).

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
├── tests/                   # Integration suite: the real gated path, no cluster
├── e2e/                     # Live matrices against a Helm-installed stack
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
A self-contained container image bundling both Python and Go runtimes together with an embedded `appa-runtime` binary. `APPA_ENABLED` selects the mode and is off by default: the image then serves the agent as the stock kagent image does and starts no runtime. With `APPA_ENABLED=true` and no `APPA_RUNTIME_URL`, the image starts `appa-runtime` on `127.0.0.1:8787` using a packaged default policy. With `APPA_RUNTIME_URL` supplied, it connects to the shared runtime service instead.

### 4. Codec Crate (`appa-adapter-kagent`)
The Rust codec crate lives at [`appa-adapter-kagent/`](../../appa-adapter-kagent) in the workspace root. It is compiled directly into `appa-runtime` and parses wire events sent by `AppaPluginKagent`.

### 5. Guide Skill (`../appa-guide/`)
The host-neutral `appa-guide` skill routes to a Claude Code or kagent reference. The demo attaches the canonical directory through kagent `gitRefs` and supplies the stock `k8s_*` tools. Applying a policy requires the kagent Approve / Reject card.

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
  --set controller.agentImage.tag=0.8.0 --wait # x-release-please-version
```

### Gate an agent

The image alone changes nothing. Turn the gate on in the Agent's own environment:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        # Optional: point at a shared appa-runtime. Left unset, the
        # quickstart image starts its bundled one on loopback.
        - name: APPA_RUNTIME_URL
          value: http://appa-runtime.kagent.svc.cluster.local:18789
```

An agent with `APPA_ENABLED=true` refuses to start without its runtime. The wrapped runtime refuses a gated start that names no `APPA_RUNTIME_URL`. The quickstart image exits when its bundled `appa-runtime` never answers. So an agent you asked to gate never runs ungated.

### Deploy the Interactive Demo

Deploy the demo chart with your OpenRouter API key:

```sh
helm upgrade --install appa-kagent-demo ./demo/chart \
  -n kagent --set openai.apiKey="$OPENROUTER_API_KEY" --wait
```

The chart sets `APPA_ENABLED=true` on every agent it renders, so the demo fleet is gated on install.

Port-forward the kagent dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:8080
```

Open [http://localhost:8901](http://localhost:8901) to explore 16 pre-seeded demonstration chats showcasing data exfiltration blocks, untrusted ingress quarantine, human-in-the-loop approvals, and multi-agent delegation.

Open `http://localhost:8901/agents/kagent/appa-guide/chat` and say `init` to inspect the installed tools and propose the fleet policy. The agent waits for chat approval before it requests the enforced approval card.

## Building from Source

To build and test the integration images locally (for example, on a local `kind` cluster):

```sh
# Build images
docker build -f appa-kagent-quickstart/Dockerfile -t appa-kagent-quickstart:dev ../../
docker build -t golang-adk:dev appa-kagent-adk-go   # the go cell: kagent derives this name for runtime: go
docker build -t appa-demo-tools:dev demo
docker build -t appa-demo-mocks:dev demo/mocks

# Load images into kind
kind load docker-image \
  appa-kagent-quickstart:dev \
  golang-adk:dev \
  appa-demo-tools:dev \
  appa-demo-mocks:dev \
  --name <cluster-name>
```

## Running Tests

### Python Unit Tests
The Python tests verify callback conversions, config validation, and wire
encoding across both supported Google ADK releases. Run both lanes from the
repository root. The locked lane resolves google-adk 2.8.0 from `uv.lock` and
skips every test that imports `kagent.adk`. The kagent v0.9.12 lane adds the
git-pinned kagent-adk with google-adk 1.31.1 and runs every test, including the
entrypoint tests and the checks that hold the ungated startup against
`kagent.adk.cli.static`:

```sh
# the locked lane: google-adk 2.8.0 from uv.lock
uv run --project integrations/kagent/appa-kagent-adk \
  pytest integrations/kagent/appa-kagent-adk/tests

# the kagent v0.9.12 lane: the pinned kagent-adk with its own resolution
uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" --with "mcp>=1.25,<2" \
  pytest integrations/kagent/appa-kagent-adk/tests
```

These are the two commands CI runs (`.github/workflows/ci.yml`);
[appa-kagent-adk/README.md](appa-kagent-adk/README.md) describes what each lane
covers.

### Go Unit Tests
The Go tests verify ADK v2 callbacks and wire format parity:

```sh
cd appa-kagent-adk-go
go test ./...
```

### Integration Suite
Twenty-two cases drive the real gated path with no cluster, no dashboard, no
model and no API key. One pytest session runs a real `appa-runtime`, the real
demo tools, the real mock externals, and a parent and a delegated child built by
the real entrypoint. It takes about thirty seconds and gates every pull request:

```sh
APPA_INTEGRATION=1 uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" --with "mcp>=1.25,<2" --with "pytest>=8" \
  pytest integrations/kagent/tests
```

### End-to-End Verification Matrices
The matrices run 17 conversations with a real model against a live cluster.
They need the demo stack above, the dashboard on `127.0.0.1:8901`, the mocks'
side channel on `127.0.0.1:8081`, and the agent under test port-forwarded for
the A2A driver. `run-matrix.sh` sets each matrix's env gate and its
dependencies. Without `APPA_A2A_E2E=1` or `APPA_UI_E2E=1` each suite skips at
import, so a bare `pytest` runs no case and exits green.

```sh
kubectl port-forward -n kagent svc/appa-demo-mocks 8081:8081 &
kubectl port-forward -n kagent svc/cluster-ops 18089:8080 &

cd integrations/kagent/e2e

# Run the A2A protocol test matrix
./run-matrix.sh python a2a

# Run the UI dashboard test matrix (requires Playwright / Chromium)
./run-matrix.sh python ui

# Every row that runs today, the go cell included, in sequence
./run-matrix.sh all
```

The go cell answers on `svc/cluster-ops-go` at `127.0.0.1:18090`. See
[e2e/README.md](e2e/README.md) for the full matrix and which rows have run.
