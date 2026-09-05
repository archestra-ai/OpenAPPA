# appa-kagent-demo

OpenAPPA on kagent, as a demo you can install in any cluster.
The chart deploys a gated `cluster-ops` fleet of three agents:
`cluster-ops`, its delegated `log-analyst`, and a `release-manager` the
policy never names. Optional Go twins are disabled by default. It deploys the shared
`appa-runtime` in one pod with mock externals, and the
demo tools. It pre-seeds every demo case as a real chat in the
dashboard (sixteen captured transcripts).
Two inputs are yours: the images (published at each release, or built
from source below) and the model key — in a value, or in the dashboard
after install.

## Prerequisites

Helm 4 and kagent 0.9.12 with its agent image set to the OpenAPPA runtime image.
That one value puts every declarative python agent in the cluster on
the OpenAPPA image. Go agents run the Go image under the name kagent
derives. Both images ship with the gate off, so every gated agent sets
`APPA_ENABLED=true` beside `APPA_RUNTIME_URL` — the chart sets both on
every agent it renders, the default fleet's three and `appa-guide`. Agents kagent
routes to `<tag>-full` stay uncovered. The release workflow publishes
both runtime images at the release version ([Images](#images)). Set this
tag to the same version as the chart's `appVersion`:

```sh
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace
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
  --set controller.agentImage.tag=0.11.0 # x-release-please-version
```

The disabled stock agents are not part of the demo and require their own provider Secret. The controller, dashboard, and tool services remain enabled.

## Install

```sh
APPA_VERSION=0.11.0 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  "https://github.com/archestra-ai/OpenAPPA/releases/download/v${APPA_VERSION}/appa-kagent-demo-${APPA_VERSION}.tgz" \
  -n kagent \
  --set-string openai.apiKey="$OPENAI_API_KEY" \
  --set agents.go.enabled=false \
  --force-conflicts \
  --wait --timeout 10m
```

Leave `openai.apiKey` unset to paste the key in the dashboard instead
(Models → appa-demo-model → Edit); the pods start once the Secret exists.
`helm upgrade` re-runs the seed Job, which is idempotent.
The default is `gpt-4.1-mini` through the OpenAI API.

The policy names the delegated children by their wire spelling,
`<namespace>__NS__<child>` with hyphens as underscores
([files/demo.appa.toml](files/demo.appa.toml)). The chart renders the
two names from the release namespace, `agents.childName` and
`agents.go.childName`. Both delegations stay declared in any namespace
and under any distinct child names. The chart fails to render when
two of these names coincide: `cluster-ops`, `release-manager`,
`agents.childName`, `agents.go.childName`, and, with
`agents.go.enabled`, `agents.go.name` and `agents.go.undeclaredName`.
The policy declares both children even without the go cell, so
`agents.go.childName` stays in the set. A child-name change rolls the
runtime pod.
The names must be DNS-1123 labels
([values.schema.json](values.schema.json)). The seeded showcase chats
are captured transcripts and keep the `kagent__NS__…` spellings of
their capture.

The seed Job posts to `kagent-controller` in the release namespace. In
another namespace set `seed.controllerUrl` to the controller address,
or set `seed.enabled=false`.

Two models answer in this chart: the agents' model
(`openai.model`, `openai.baseUrl`) and the model the policy's
sanitizers consult (`llm.model`, `llm.url`). Both read the same Secret
— the agents through the ModelConfig, the runtime as
`APPA_LLM_API_KEY` — so one key serves both. Point the pair at any
OpenAI-compatible endpoint, as
[../../e2e/ci](../../e2e/ci/) does for the live A2A matrix in CI. The
sanitizers ask for a `json_schema` answer, so their model must support
structured outputs.

| Value | Default | Meaning |
|---|---|---|
| `openai.apiKey` | `""` | The OpenAI key; lands in the Secret named after the ModelConfig. |
| `openai.model` | `gpt-4.1-mini` | The agents' model. |
| `openai.existingSecret` | `""` | Use an existing Secret with `OPENAI_API_KEY` instead. |
| `openai.baseUrl` | `""` | Optional OpenAI-compatible endpoint for the agents' model. |
| `runtime.image.*` | `ghcr.io/archestra-ai/appa-runtime:<appVersion>` | The shared runtime image. Published at each release version. |
| `runtime.reasoningEffort` | `""` | Optionally fills `reasoning_effort` for the OpenAI model when the CRD cannot. |
| `runtime.persistence.enabled` | `false` | Keep trajectories on a PersistentVolume. |
| `runtime.persistence.migrateOwnership` | `false` | Run a one-time root init container to repair a retained PVC owned by another UID. Keep this off on fresh or restricted deployments. |
| `llm.model` | `gpt-4.1-mini` | The model the policy's sanitizers consult (`[externals.llm]`). |
| `llm.url` | `""` | Optional OpenAI-compatible endpoint for that model. |
| `mocks.approvalWindowSeconds` | `25` | How long the change board waits for a ruling, inside the policy's `externals.timeout_ms` (30 s). |
| `seed.enabled` | `true` | Replay the showcase chats into `cluster-ops` after install. The go twin gets none. |
| `seed.controllerUrl` | `""` | The kagent controller the seed Job posts to. Empty means `kagent-controller` in the release namespace. |
| `agents.childName` | `log-analyst` | The python child `cluster-ops` delegates to. The policy names it `<namespace>__NS__<childName>`, hyphens as underscores. |
| `agents.go.enabled` | `false` | Also render the go cell: `cluster-ops-go`, `log-analyst-go`, and `release-manager-go` on kagent's go runtime (needs the published `golang-adk` alias beside `appa-kagent-adk`). |
| `agents.go.childName` | `log-analyst-go` | The go child `cluster-ops-go` delegates to. The policy names it the same way. |
| `guide.enabled` | `true` | Install the `appa-guide` agent: the routing skill over the kagent tool server's k8s tools, gated by the shared runtime. |
| `guide.skill.git.*` | this repo, `v<appVersion>`, `integrations/appa-guide` | Where kagent clones the canonical skill. An empty ref pins the release tag; set a ref explicitly for development. Claude packaging stages the same directory into its plugin. The cluster must reach the repo (or a fork). |
| `guide.toolServer` | `kagent-tool-server` | The RemoteMCPServer serving the `k8s_*` tools. |

kagent compiles an agent that calls another agent as a tool only once
that agent exists, and it does not retry on its own when the child
appears later. The chart renders each child before its parent, which
keeps a fresh install in order; if a parent ever reports
`ReconcileFailed … not found`, a spec change (or `helm upgrade`) makes
the controller compile it again.

## What is inside

```
three agents (cluster-ops, log-analyst, and release-manager; optional Go twins)
                                  ──APPA_RUNTIME_URL──▶ Service appa-runtime:18787
                                                         │ pod appa-runtime
                                                         ├─ runtime (appa)    0.0.0.0:18787, policy from ConfigMap
                                                         └─ mocks             0.0.0.0:8081, reached at 127.0.0.1:8081 —
                                                                              annotator, release window, change board
                                                                              (+ Service appa-demo-mocks)
demo-tools (Deployment + RemoteMCPServer) ◀── the agents' MCP tools
appa-guide Agent ── appa-guide skill (gitRefs) + kagent-tool-server k8s tools
                    ──APPA_RUNTIME_URL──▶ the shared runtime (HITL on apply)
seed Job (post-install) ──▶ kagent-controller /api/sessions, /api/tasks
```

The runtime serves agents directly on `0.0.0.0:18787`. Its URL externals
remain loopback-only, so the mocks stay in the same pod.

## Images

The demo uses five images published to `ghcr.io/archestra-ai` at the
release version: `appa-runtime`, `appa-kagent-adk`,
`appa-kagent-adk-go`, `appa-demo-tools`, and `appa-demo-mocks`.
`appa-runtime` supports `linux/amd64` and `linux/arm64`. The other four
support `linux/amd64`. The release also publishes
`golang-adk` at that version, on the same digest as
`appa-kagent-adk-go`. kagent's controller derives the Go runtime image
under that name.

The chart's image defaults name `appa-runtime`,
`appa-demo-tools` and `appa-demo-mocks` in that registry, at the
chart's `appVersion`. Each release sets `appVersion` to the version it
publishes, so the defaults name images the registry holds. Point the
image values at another release version, or at your own registry, to
run a tree no release published (or load the images into kind, below).

## On kind, from source

```sh
docker build -f appa-runtime/Dockerfile -t appa-runtime:dev .
docker build -t appa-kagent-adk:dev integrations/kagent/appa-kagent-adk
docker build -t appa-demo-tools:dev integrations/kagent/demo
docker build -t appa-demo-mocks:dev integrations/kagent/demo/mocks
docker build -t golang-adk:dev integrations/kagent/appa-kagent-adk-go   # the go cell: kagent derives this name
kind load docker-image appa-runtime:dev appa-kagent-adk:dev appa-demo-tools:dev appa-demo-mocks:dev golang-adk:dev --name <cluster>
APPA_VERSION=0.11.0 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  "https://github.com/archestra-ai/OpenAPPA/releases/download/v${APPA_VERSION}/appa-kagent-demo-${APPA_VERSION}.tgz" \
  -n kagent \
  --set openai.apiKey="$OPENAI_API_KEY" \
  --set runtime.image.repository=docker.io/library/appa-runtime --set runtime.image.tag=dev --set runtime.image.pullPolicy=Never \
  --set tools.image.repository=docker.io/library/appa-demo-tools --set tools.image.tag=dev --set tools.image.pullPolicy=Never \
  --set mocks.image.repository=docker.io/library/appa-demo-mocks --set mocks.image.tag=dev --set mocks.image.pullPolicy=Never
```

(with kagent's `controller.agentImage` set to `docker.io/library/appa-kagent-adk:dev`, `pullPolicy: Never`.)

## Verify

The matrices in [../../e2e/ui](../../e2e/ui/) and [../../e2e/a2a](../../e2e/a2a/)
run against this install: port-forward `svc/kagent-ui` (8901) and, for the
change-board cases, `svc/appa-demo-mocks` (8081). They read the release
namespace and the child names from `APPA_NAMESPACE`, `APPA_CHILD` and
`APPA_UNDECLARED`, with this chart's defaults.

[../../e2e/ci](../../e2e/ci/) installs this chart on a kind cluster
from the locally built images and runs all 18 A2A cases against
it, on a laptop or on a CI runner.

[tests/render-test.sh](tests/render-test.sh) renders the chart with
helm alone, no cluster: the defaults, the refused name collisions, the
go cell off, and scalar-looking names quoted. CI runs it after
`helm lint`.
