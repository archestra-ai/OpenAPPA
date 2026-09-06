# OpenAPPA for kagent

This directory contains the OpenAPPA integration for [kagent](https://github.com/kagent-dev/kagent), the Kubernetes-native AI agent orchestrator.

OpenAPPA gates declarative kagent agents through a shared `appa-runtime` service. The integration requires no forks of kagent or Google ADK.

- **Public documentation**: [website/content/docs/kagent.md](../../website/content/docs/kagent.md)
- **Demo scenarios**: [demo/SCENARIOS.md](demo/SCENARIOS.md)
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
├── tests/                   # Integration suite: gated execution without a cluster
├── e2e/                     # Live matrices against a Helm-installed stack
│   ├── a2a/                 # Matrix tests over the A2A protocol
│   └── ui/                  # Browser matrix tests driving the kagent dashboard
├── examples/                # Reference policies (e.g. kagent.appa.toml)
├── fixtures/                # Canonical wire event fixtures shared across languages
└── IMPLEMENTATION.md        # Technical architecture and wire specifications
```

### 1. Python Runtime (`appa-kagent-adk/`)
Wraps kagent's published Python runtime container image. It ships `AppaPluginKagent`, a Google ADK `BasePlugin` that maps lifecycle callbacks to OpenAPPA `/hook` events. It appends the plugin and the `execute_remedy_plan` tool to the agent entrypoint.

### 2. Go Runtime (`appa-kagent-adk-go/`)
Implements `AppaPluginKagent` for Google Go ADK v2. It provides a replacement runtime main that registers the plugin, manages session lineage headers across delegations, and coordinates human-in-the-loop approvals.

### 3. Codec Crate (`appa-adapter-kagent`)
The Rust codec crate lives at [`appa-adapter-kagent/`](../../appa-adapter-kagent) in the workspace root. It compiles directly into `appa-runtime` and parses wire events sent by `AppaPluginKagent`.

### 4. Guide Skill (`../appa-guide/`)
The `appa-guide` agent runs in kagent using Kubernetes tools and `appa_match_batteries`. It drafts policy in chat and updates the runtime ConfigMap under kagent confirmation cards.

## Quickstart

These commands require Helm v4 to support server-side apply.

### 1. Install kagent with the OpenAPPA plugin

Set your OpenAI API key:

```sh
export OPENAI_API_KEY="<your-api-key>"
```

Deploy the CRDs, provider secret, and controller:

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
```

### 2. Deploy appa-runtime

```sh
helm upgrade --install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n appa --create-namespace \
  --set persistence.enabled=true \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
```

### 3. Deploy demo fixtures

```sh
helm upgrade --install appa-kagent-demo \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n kagent \
  --set-string runtime.url=http://appa-runtime.appa.svc.cluster.local:18787 \
  --set-string modelConfig.name=default-model-config \
  --set-string runtime.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
```

### 4. Enable gating on an agent

Set `APPA_ENABLED=true` and provide the runtime URL:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
metadata:
  name: sre-agent
  namespace: kagent
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: "http://appa-runtime.appa.svc.cluster.local:18787"
```

If `APPA_RUNTIME_URL` is unreachable, tool calls stop fail-closed before execution.

### 5. Open the interactive demo

Forward the dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

1. Open `http://localhost:8080/agents/kagent/appa-guide/chat` and send `init`.
2. Review the proposed policy and approve the confirmation card.
3. Open `cluster-ops` to run the demonstration scenarios. See [demo/SCENARIOS.md](demo/SCENARIOS.md).

## Building from source

To build and load images locally into a `kind` cluster:

```sh
# Build images
docker build -f ../../appa-runtime/Dockerfile -t appa-runtime:dev ../../
docker build -t appa-kagent-adk:dev appa-kagent-adk
docker build -t golang-adk:dev appa-kagent-adk-go
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

## Running tests

### Python Unit Tests

The unit tests verify callback conversions and wire encoding across supported Google ADK versions:

```sh
# Run tests with locked dependencies
uv run --project integrations/kagent/appa-kagent-adk \
  pytest integrations/kagent/appa-kagent-adk/tests

# Run tests with kagent-adk 0.9.12
uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" --with "mcp>=1.25,<2" \
  pytest integrations/kagent/appa-kagent-adk/tests
```

### Go Unit Tests

Verify Go ADK v2 callbacks and wire parity:

```sh
cd appa-kagent-adk-go
go test ./...
```

### Integration Suite

The integration suite runs twenty-two policy scenarios with no external model API:

```sh
APPA_INTEGRATION=1 uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" --with "mcp>=1.25,<2" --with "pytest>=8" \
  pytest integrations/kagent/tests
```

### End-to-End Verification Matrices

Live verification matrices run against a cluster stack:

```sh
kubectl port-forward -n kagent svc/appa-demo-mocks 8081:8081 &
kubectl port-forward -n kagent svc/cluster-ops 18089:8080 &

cd integrations/kagent/e2e

# Run A2A protocol test matrix
./run-matrix.sh python a2a

# Run UI dashboard test matrix
./run-matrix.sh python ui

# Run all suites
./run-matrix.sh all
```
