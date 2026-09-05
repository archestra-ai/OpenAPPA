# OpenAPPA for kagent

This directory contains the OpenAPPA integration for [kagent](https://github.com/kagent-dev/kagent), the Kubernetes-native AI agent orchestrator.

OpenAPPA gates declarative kagent Agents through one remote `appa-runtime`. Set `controller.agentImage` to the Python adapter image. Each protected Agent then sets `APPA_ENABLED=true` and a nonempty `APPA_RUNTIME_URL`. Without that flag, the image preserves stock behavior. This needs no kagent or Google ADK fork.

- **Operator guide**: [website/content/docs/kagent.md](../../website/content/docs/kagent.md)
- **Implementation specification**: [IMPLEMENTATION.md](IMPLEMENTATION.md)
- **Shared runtime Helm chart**: [charts/appa-runtime/README.md](../../charts/appa-runtime/README.md)
- **Demo Helm chart**: [demo/chart/README.md](demo/chart/README.md)

## Components

```text
integrations/kagent/
├── appa-kagent-adk/         # Python ADK plugin and entrypoint (appa_kagent_adk)
├── appa-kagent-adk-go/      # Go ADK plugin and replacement runtime main
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

### 3. Codec Crate (`appa-adapter-kagent`)
The Rust codec crate lives at [`appa-adapter-kagent/`](../../appa-adapter-kagent) in the workspace root. It is compiled directly into `appa-runtime` and parses wire events sent by `AppaPluginKagent`.

### 4. Guide Skill (`../appa-guide/`)
The host-neutral `appa-guide` skill routes to a Claude Code or kagent reference. In kagent, it runs as a dedicated declarative `Agent` using the stock `k8s_*` tools from the kagent tool server to inspect installed tools, match batteries, propose policies in chat, and apply them to the runtime ConfigMap under kagent's Approve / Reject card. With persistence enabled, it also verifies and refreshes upstream policy batteries without container rebuilds. See [website/content/docs/kagent.md](../../website/content/docs/kagent.md) for full deployment and maintenance workflows.

## Quickstart

These commands require Helm 4 because upgrades reclaim chart-owned fields with server-side apply.

### Deploy kagent with OpenAPPA

Install the CRDs and remote runtime before the kagent controller uses the OpenAPPA adapter image:

```sh
# 1. Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace --force-conflicts

# 2. Install the remote runtime
APPA_VERSION=0.10.0 # x-release-please-version
helm upgrade --install appa-runtime oci://ghcr.io/archestra-ai/charts/appa-runtime \
  --version "$APPA_VERSION" -n appa --create-namespace \
  --set persistence.enabled=true \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --force-conflicts --wait --timeout 10m

# 3. Install kagent controller with the OpenAPPA adapter image
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-adk \
  --set k8s-agent.enabled=false \
  --set kgateway-agent.enabled=false \
  --set istio-agent.enabled=false \
  --set promql-agent.enabled=false \
  --set observability-agent.enabled=false \
  --set argo-rollouts-agent.enabled=false \
  --set helm-agent.enabled=false \
  --set cilium-policy-agent.enabled=false \
  --set cilium-manager-agent.enabled=false \
  --set cilium-debug-agent.enabled=false \
  --force-conflicts \
  --wait --timeout 10m \
  --set controller.agentImage.tag="$APPA_VERSION"
```

The stock agents are not part of this quickstart and require a separate provider Secret such as `kagent-openai`. These flags disable them while retaining the controller, dashboard, and tool services used below.

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
        - name: APPA_RUNTIME_URL
          value: http://appa-runtime.appa.svc.cluster.local:18787
```

An Agent with `APPA_ENABLED=true` refuses to start without a reachable remote runtime. It never falls back to ungated execution.

### Deploy a shared cluster-wide runtime

The production chart installs one directly reachable `appa-runtime`, the release batteries, and optional persistence for trajectory auditing and battery refreshing:

```sh
helm upgrade --install appa-runtime ../../charts/appa-runtime -n kagent \
  --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi
```

Point any declarative Agent at `http://appa-runtime.kagent.svc.cluster.local:18787`. `GET /batteries` on that Service lists the batteries bundled with this release. The `appa-guide` skill translates matched declarations to exact kagent tool names, writes the policy ConfigMap under the kagent Approve / Reject card, and hot-reloads the policy. With persistence enabled, `appa-guide` can also inspect, verify, and refresh upstream batteries from published releases without rebuilding containers.

### Deploy the Interactive Demo

Deploy the demo chart with your OpenRouter API key:

```sh
APPA_VERSION=0.10.0 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  "https://github.com/archestra-ai/OpenAPPA/releases/download/v${APPA_VERSION}/appa-kagent-demo-${APPA_VERSION}.tgz" \
  -n kagent \
  --set-string openai.apiKey="$OPENAI_API_KEY" \
  --set agents.go.enabled=false \
  --force-conflicts \
  --wait --timeout 10m
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
docker build -f ../../appa-runtime/Dockerfile -t appa-runtime:dev ../../
docker build -t appa-kagent-adk:dev appa-kagent-adk
docker build -t golang-adk:dev appa-kagent-adk-go   # the go cell: kagent derives this name for runtime: go
docker build -t appa-demo-tools:dev demo
docker build -t appa-demo-mocks:dev demo/mocks

# Load images into kind
kind load docker-image \
  appa-runtime:dev \
  appa-kagent-adk:dev \
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
The matrices run 18 conversations with a real model against a live cluster.
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
