# The demo on a cluster — Helm first

The demo is a Helm chart, [chart/](chart/). It installs a gated
`cluster-ops` fleet with a delegated `log-analyst` and a
`release-manager` the policy never names, so every delegation to it is
denied. It also installs the go twins of all three (`cluster-ops-go`,
`log-analyst-go`, `release-manager-go`). The shared `appa-runtime` runs
in one pod with its relay and mock externals. The chart adds the demo
tools and pre-seeds every demo case as a real chat in the kagent
dashboard.

Install it into any cluster that runs kagent 0.9.12 on the OpenAPPA
runtime image. Two inputs are yours: the images ([chart/README.md](chart/README.md)) and the model key. Install
it into any namespace. The chart renders the wire names of the delegated
children from the release namespace and `agents.childName` /
`agents.go.childName` ([chart/README.md](chart/README.md)).

```sh
# kagent on the OpenAPPA runtime image, fleet-wide (once per cluster).
# The image gates nothing by itself: every agent opts in with
# APPA_ENABLED=true, and the demo chart sets it on every agent it renders.
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
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
  --wait --timeout 10m \
  --set controller.agentImage.tag=0.9.0 # x-release-please-version

# the demo
APPA_VERSION=0.9.0 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  "https://github.com/archestra-ai/OpenAPPA/releases/download/v${APPA_VERSION}/appa-kagent-demo-${APPA_VERSION}.tgz" \
  -n kagent \
  --set-string openai.apiKey="$OPENAI_API_KEY" \
  --wait --timeout 10m
kubectl -n kagent port-forward svc/kagent-ui 8901:8080
```

Then open `http://localhost:8901/agents/kagent/cluster-ops/chat`. Say
`init` in `http://localhost:8901/agents/kagent/appa-guide/chat` to
inventory cluster MCP tools and propose fleet policy; the apply raises
the kagent Approve card, and a new gated chat uses the reloaded policy. The
showcase chats are already there — replay them in
[SCENARIOS.md](SCENARIOS.md) terms, or run your own: the exfiltration
ask is denied at the secret read and leaks nothing, the crash-log
injection is gated at ingress, the restart asks a person through
kagent's Approve/Reject card (Approve runs it, Reject leaves it
blocked), the rollback waits on the remote change board, and the
ordinary reads flow untouched. The agent takes every other remedy
itself — its instruction steers it to the sanitized result by default,
and the chat can steer it to accept the change.

That one `controller.agentImage` value puts every python-runtime
declarative agent in the cluster on the quickstart image. Every
`runtime: go` agent runs on the `golang-adk` name that kagent derives
from it. kagent's stock sample agents (k8s, helm, istio, cilium, and
observability) are disabled because the demo does not use them and they
require a separate provider Secret. The demo agents set `APPA_ENABLED`
themselves and point at the shared runtime, so a parent and its delegated
child land in one trajectory.

The bundled runtime loads the packaged policy
([../examples/kagent.appa.toml](../examples/kagent.appa.toml)). That
policy names seven demo tools, `ask_user`, and the entrypoint's two
synthetic tools, and it carries no wildcard. So the runtime refuses at
`ToolCall` every other tool call a gated sample agent makes. To gate a
sample agent, set `APPA_ENABLED: "true"` in its
`spec.declarative.deployment.env` and mount a policy that names its
tools over `APPA_CONFIG`, or add a wildcard annotator. Only with the
knob on does the entrypoint start the bundled runtime and load that
policy. To gate a whole fleet at once, bake `ENV APPA_ENABLED=true`
into a derived image and point `controller.agentImage` at it.

## What the chart deploys

The shared runtime serves the full-matrix policy
([chart/files/demo.appa.toml](chart/files/demo.appa.toml)): real
sanitizers over `[externals.llm]`, the `runbook-readers` annotator, the
human-less `release-window` authority and the remote `change-board`
authority (people out of band, ruling on the mock's side channel)
answered by the mock externals ([mocks/](mocks/)), the delegated child
as the spawn, and the `oncall` human-review authority.

`appa-runtime` binds loopback only, by design, and a `url` binding in
its policy takes cleartext http to loopback only. So the runtime pod
carries three containers. The runtime listens on `127.0.0.1:18787`. An
nginx relay on `:18789` fronts it: every agent reaches the relay
through the `appa-runtime` Service, and the relay rewrites `Host` to a
loopback value.
The runtime's `/mcp` — the rmcp server behind the remedy tool —
validates that value. The third container is the mocks. They bind
`0.0.0.0:8081`, and the runtime reaches them at `127.0.0.1:8081`. The
mocks' side channel is also a Service (`appa-demo-mocks`), so a board
member outside the pod — or the e2e matrices — can rule.

The `demo-tools` image builds from [Dockerfile](Dockerfile), the mocks
from [mocks/Dockerfile](mocks/Dockerfile); the chart README covers
building and loading them on kind.

## The scenarios, scripted

A deterministic variant of the same cases drives a real ADK agent loop
with a scripted model through the real plugin and a local runtime — no
cluster, no model key: [SCENARIOS.md](SCENARIOS.md) and
[../tests/](../tests/). Those twenty-two tests run on
[../tests/policy.appa.toml](../tests/policy.appa.toml), the full-matrix
policy above with both sanitizers rebound to the mock's `/sanitize`, so
the derivation is deterministic. `APPA_INTEGRATION=1 … pytest
integrations/kagent/tests` runs them; [../tests/README.md](../tests/README.md)
carries the full line. The matrices in [../e2e/](../e2e/) drive the same
substance with a real model, and need the cluster and the key.
