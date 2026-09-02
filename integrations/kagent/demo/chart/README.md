# appa-kagent-demo

OpenAPPA on kagent, as a demo you can install in any cluster: a gated
`cluster-ops` fleet with a delegated `log-analyst`, the shared
`appa-runtime` in one pod with its relay and mock externals, the demo
tools, and every demo case pre-seeded as a real chat in the dashboard
(sixteen captured transcripts).
Only the model key is yours to supply — in a value, or in the dashboard
after install.

## Prerequisites

kagent 0.9.12 with its agent image set to the OpenAPPA runtime image.
That one value gates every declarative agent in the cluster:

```sh
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.7.0 --wait
```

## Install

```sh
helm upgrade --install appa-kagent-demo ./integrations/kagent/demo/chart \
  -n kagent --set openai.apiKey="$OPENAI_API_KEY" --wait
```

Leave `openai.apiKey` unset to paste the key in the dashboard instead
(Models → appa-demo-model → Edit); the pods start once the Secret exists.
`helm upgrade` re-runs the seed Job, which is idempotent.

| Value | Default | Meaning |
|---|---|---|
| `openai.apiKey` | `""` | The provider key; lands in the Secret named after the ModelConfig. |
| `openai.model` | `gpt-5.6-luna` | The agents' model. |
| `openai.existingSecret` | `""` | Use an existing Secret with `OPENAI_API_KEY` instead. |
| `runtime.image.*` | `ghcr.io/archestra-ai/appa-kagent-quickstart:<appVersion>` | The runtime image (also the agents' image, via kagent). |
| `runtime.reasoningEffort` | `none` | Fills `reasoning_effort` for the OpenAI model when the CRD cannot. |
| `runtime.persistence.enabled` | `false` | Keep trajectories on a PersistentVolume. |
| `mocks.approvalWindowSeconds` | `25` | How long the change board waits for a ruling. |
| `seed.enabled` | `true` | Replay the showcase chats after install. |
| `agents.go.enabled` | `true` | Also run `cluster-ops-go`, the same agent on kagent's go runtime (needs the `golang-adk` image beside the python one). |

kagent compiles an agent that calls another agent as a tool only once
that agent exists, and it does not retry on its own when the child
appears later. The chart renders each child before its parent, which
keeps a fresh install in order; if a parent ever reports
`ReconcileFailed … not found`, a spec change (or `helm upgrade`) makes
the controller compile it again.

## What is inside

```
agents (cluster-ops, log-analyst) ──APPA_RUNTIME_URL──▶ Service appa-runtime:18789
                                                        │ pod appa-runtime
                                                        ├─ relay (nginx)     :18789 → 127.0.0.1:18787, Host rewritten
                                                        ├─ runtime (appa)    127.0.0.1:18787, policy from ConfigMap
                                                        └─ mocks             127.0.0.1:8081 — annotator, release window,
                                                                              change board (+ Service appa-demo-mocks)
demo-tools (Deployment + RemoteMCPServer) ◀── the agents' MCP tools
seed Job (post-install) ──▶ kagent-controller /api/sessions, /api/tasks
```

The runtime binds loopback only and its URL externals must be loopback,
so the relay and the mocks are its sidecars (the plan's
[Demo chart](../../IMPLEMENTATION.md#demo-chart) section).

## Images

The defaults name `ghcr.io/archestra-ai/appa-kagent-quickstart`,
`appa-demo-tools` and `appa-demo-mocks` at the chart's `appVersion`.
Publishing them is a release step of this repository; until a tag is
published, build them from source and point the image values at your
registry (or load them into kind, below).

## On kind, from source

```sh
docker build -f integrations/kagent/appa-kagent-quickstart/Dockerfile -t appa-kagent-quickstart:dev .
docker build -t appa-demo-tools:dev integrations/kagent/demo
docker build -t appa-demo-mocks:dev integrations/kagent/demo/mocks
docker build -t golang-adk:dev integrations/kagent/appa-kagent-adk-go   # the go cell: kagent derives this name
kind load docker-image appa-kagent-quickstart:dev appa-demo-tools:dev appa-demo-mocks:dev golang-adk:dev --name <cluster>
helm upgrade --install appa-kagent-demo ./integrations/kagent/demo/chart -n kagent \
  --set openai.apiKey="$OPENAI_API_KEY" \
  --set runtime.image.repository=docker.io/library/appa-kagent-quickstart --set runtime.image.tag=dev --set runtime.image.pullPolicy=Never \
  --set tools.image.repository=docker.io/library/appa-demo-tools --set tools.image.tag=dev --set tools.image.pullPolicy=Never \
  --set mocks.image.repository=docker.io/library/appa-demo-mocks --set mocks.image.tag=dev --set mocks.image.pullPolicy=Never
```

(with kagent's `controller.agentImage` set to `docker.io/library/appa-kagent-quickstart:dev`, `pullPolicy: Never`.)

## Verify

The matrices in [../../e2e/ui](../../e2e/ui/) and [../../e2e/a2a](../../e2e/a2a/)
run against this install: port-forward `svc/kagent-ui` (8901) and, for the
change-board cases, `svc/appa-demo-mocks` (8081).
