# The demo on a cluster — Helm first

The demo is a Helm chart, [chart/](chart/): a gated `cluster-ops` fleet
with a delegated `log-analyst` and a `release-manager` the policy never
names (so every delegation to it is denied), the shared `appa-runtime` in one pod
with its relay and mock externals, the demo tools, and every demo case
pre-seeded as a real chat in the kagent dashboard. Install it into any
cluster that runs kagent 0.9.12 on the OpenAPPA runtime image; the only
input that is yours is the model key.

```sh
# kagent, gated fleet-wide through one image value (once per cluster)
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.7.0 --wait

# the demo
helm upgrade --install appa-kagent-demo ./integrations/kagent/demo/chart \
  -n kagent --set openai.apiKey="$OPENAI_API_KEY" --wait
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

That one `controller.agentImage` value gates the whole fleet: the
chart's stock sample agents (k8s, helm, istio, cilium, observability,
…) all come up on the quickstart image, each with its bundled
`appa-runtime` healthy — zero agent changes anywhere. The demo agents
point at the shared runtime instead, so a parent and its delegated
child land in one trajectory.

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
carries three containers: the runtime on `127.0.0.1:18787`, an nginx
relay on `:18789` that every agent reaches through the `appa-runtime`
Service and that rewrites `Host` to a loopback value (the runtime's
`/mcp` — the rmcp server behind the remedy tool — validates it), and
the mocks on `127.0.0.1:8081`. The mocks' side channel is also a
Service (`appa-demo-mocks`), so a board member outside the pod — or the
e2e matrices — can rule.

The `demo-tools` image builds from [Dockerfile](Dockerfile), the mocks
from [mocks/Dockerfile](mocks/Dockerfile); the chart README covers
building and loading them on kind.

## The scenarios, scripted

A deterministic variant of the same cases drives a real ADK agent loop
with a scripted model through the real plugin and a local runtime on
the example policy — no cluster, no model key:
[SCENARIOS.md](SCENARIOS.md) and [../e2e/](../e2e/).
