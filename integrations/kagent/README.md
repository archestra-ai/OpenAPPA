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
The host-neutral `appa-guide` skill routes to a Claude Code or kagent reference. In kagent, it runs as a dedicated declarative `Agent` using the stock `k8s_*` tools to inspect the cluster and runtime-owned `appa_match_batteries` to compute exact battery matches. It proposes policy in chat and applies it to the runtime ConfigMap under kagent's Approve / Reject card. With persistence enabled, it also verifies and refreshes upstream policy batteries without container rebuilds. See [website/content/docs/kagent.md](../../website/content/docs/kagent.md) for full deployment and maintenance workflows.

## Quickstart

These commands require Helm 4 because upgrades reclaim chart-owned fields with server-side apply.

### Install kagent with appa plugin

Install the CRDs and kagent controller before appa creates its configuring Agent:

Set your provider credential first. Replace the placeholder; do not run this line unchanged:

```sh
export OPENAI_API_KEY="<your-api-key>"
```

Then run the complete install:

```sh
: "${OPENAI_API_KEY:?Set OPENAI_API_KEY before installing kagent}"

# 1. Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace --force-conflicts

# 2. Install kagent with the appa plugin image
APPA_VERSION=0.14.0 # x-release-please-version
OPENAI_API_KEY_B64="$(printf %s "$OPENAI_API_KEY" | base64 | tr -d '\n')"
kubectl apply -f - <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: kagent-openai
  namespace: kagent
type: Opaque
data:
  OPENAI_API_KEY: $OPENAI_API_KEY_B64
EOF
unset OPENAI_API_KEY_B64

helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \
  --set providers.default=openAI \
  --set-string providers.openAI.apiKeySecretRef=kagent-openai \
  --set-string providers.openAI.apiKeySecretKey=OPENAI_API_KEY \
  --set-string providers.openAI.model=gpt-5.6-luna \
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
  --set grafana-mcp.enabled=false \
  --set querydoc.enabled=false \
  --force-conflicts \
  --wait --timeout 10m \
  --set controller.agentImage.tag="v$APPA_VERSION"

# 3. Install appa
helm upgrade --install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n appa --create-namespace \
  --set persistence.enabled=true \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m

# 4. Install the demo fixtures
helm upgrade --install appa-kagent-demo \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n kagent \
  --set-string runtime.url=http://appa-runtime.appa.svc.cluster.local:18787 \
  --set-string modelConfig.name=default-model-config \
  --set-string runtime.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
```

The stock agents are not part of this quickstart. These flags disable them while retaining the controller, dashboard, and tool services. The provider values create `default-model-config` with `gpt-5.6-luna` on the OpenAI API. `appa-guide` and every demo Agent use that configuration. The adapter supplies `reasoning_effort: "none"`, which Luna requires for function tools on the chat completions API.

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

An Agent with `APPA_ENABLED=true` refuses startup when `APPA_RUNTIME_URL` is empty. It can report Ready before contacting that URL. If the runtime is unreachable, the first gated callback fails and no tool runs; the Agent never falls back to ungated execution.

### Deploy a shared cluster-wide runtime

The production chart installs one directly reachable `appa-runtime`, the release batteries, and optional persistence for trajectory auditing and battery refreshing:

```sh
helm upgrade --install appa-runtime ../../charts/appa-runtime -n kagent \
  --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi
```

Point any declarative Agent at `http://appa-runtime.kagent.svc.cluster.local:18787`. `GET /batteries` on that Service lists the batteries bundled with this release. The `appa-guide` skill translates matched declarations to exact kagent tool names, writes the policy ConfigMap under the kagent Approve / Reject card, and hot-reloads the policy. With persistence enabled, `appa-guide` can also inspect, verify, and refresh upstream batteries from published releases without rebuilding containers.

### Open the interactive demo

The fixture chart sets `APPA_ENABLED=true` on every Agent it renders and points each one at the runtime owned by `appa-runtime`. Its canned `mcp__github__get_file_contents` and `mcp__github__issue_write` tools match the shipped GitHub battery exactly, so `appa-guide init` proposes a real battery include rather than demo-only policy.

Port-forward the kagent dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:8080
```

Open [http://localhost:8901](http://localhost:8901) to explore 16 pre-seeded demonstration chats showcasing data exfiltration blocks, untrusted ingress quarantine, human-in-the-loop approvals, and multi-agent delegation.

Open `http://localhost:8901/agents/kagent/appa-guide/chat` and say `init`. The guide verifies the demo's inert policy template and proposes the fleet policy. It waits for chat approval before requesting the enforced approval card; the demo chart never writes serving policy itself.

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
