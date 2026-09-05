# appa-kagent-demo

Fixture-only OpenAPPA demo for kagent. The chart installs:

- the gated `cluster-ops`, `log-analyst`, and `release-manager` Agents;
- optional Go twins;
- the `demo-tools` MCP server;
- deterministic mock implementations for demo policy components;
- an inert, rendered policy template;
- sixteen seeded dashboard chats.

The chart does not install `appa-runtime`, serving policy, persistence,
provider credentials, a `ModelConfig`, or `appa-guide`. The dedicated
`appa-runtime` and kagent releases own those resources.

## Prerequisites

Install kagent with `appa-kagent-adk` and a provider-backed ModelConfig.
Install the `appa-runtime` chart with `appaGuide.enabled=true`. The public
[kagent guide](https://openappa.com/kagent) has the complete sequence.

The defaults expect:

- runtime: `http://appa-runtime.appa.svc.cluster.local:18787`;
- ModelConfig: `default-model-config` in the demo namespace;
- appa-guide: `appa-guide` in the demo namespace.

## Install

```sh
APPA_VERSION=0.12.0 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  oci://ghcr.io/archestra-ai/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n kagent \
  --set-string runtime.url=http://appa-runtime.appa.svc.cluster.local:18787 \
  --set-string modelConfig.name=default-model-config \
  --force-conflicts --wait --timeout 10m
```

Open `appa-guide` and send `init`. The guide verifies this release and
reads `ConfigMap/appa-kagent-demo-policy`. It presents the resulting
behavior before copying any entries into the runtime-owned policy. Reply
with approval, then approve the enforced kagent confirmation card.

The demo ConfigMap is never mounted or served directly. Installing or
upgrading this chart cannot change runtime policy.

## Values

| Value | Default | Meaning |
|---|---|---|
| `runtime.url` | `http://appa-runtime.appa.svc.cluster.local:18787` | Existing shared runtime used by every demo Agent. |
| `runtime.reasoningEffort` | `""` | Optional reasoning effort passed to each Agent model request. |
| `modelConfig.name` | `default-model-config` | Existing kagent ModelConfig used by every demo Agent. |
| `tools.image.*` | `ghcr.io/archestra-ai/appa-demo-tools:<appVersion>` | Demo MCP server image. |
| `mocks.image.*` | `ghcr.io/archestra-ai/appa-demo-mocks:<appVersion>` | Demo policy-service image. |
| `mocks.approvalWindowSeconds` | `25` | Change-board ruling window, below the policy's 30-second consult timeout. |
| `seed.enabled` | `true` | Replay the sixteen showcase chats after install. |
| `seed.controllerUrl` | controller in the release namespace | kagent controller receiving seeded sessions. |
| `agents.childName` | `log-analyst` | Python child named by the rendered delegation contract. |
| `agents.go.enabled` | `false` | Also install the three Go demo Agents. |
| `agents.go.childName` | `log-analyst-go` | Go child named by the rendered delegation contract. |

Every Agent name must be a DNS-1123 label. The chart refuses collisions
among fixed and configurable names. kagent spells delegated Agents as
`<namespace>__NS__<name>`, with hyphens changed to underscores. The
policy template renders those exact names.

## Ownership

```text
appa-runtime release (namespace appa)
  runtime Deployment + Service
  serving policy ConfigMap + persistence
  appa-guide Agent (namespace kagent)

appa-kagent-demo release (namespace kagent)
  cluster-ops fleet ──APPA_RUNTIME_URL──▶ shared runtime
  demo-tools Deployment + Service + RemoteMCPServer
  appa-demo-mocks Deployment + Service
  inert policy-template ConfigMap
  seed Job ──▶ kagent-controller
```

The policy uses fixed local command adapters in the runtime image to
forward consult envelopes to `appa-demo-mocks`. This keeps cleartext
demo traffic out of URL bindings, which accept HTTP only on loopback.
The mock service returns deterministic Annotator, Authority, and sanitizer
answers and exposes the change board at `/pending` and `/decide`.

## Images

This chart directly uses only `appa-demo-tools` and `appa-demo-mocks`.
Both default to the chart `appVersion` in `ghcr.io/archestra-ai`. kagent
and the runtime releases separately select `appa-kagent-adk`,
`appa-kagent-adk-go`/`golang-adk`, and `appa-runtime`.

For a local kind stack, build and load the four images, then install the
two charts with local image overrides:

```sh
docker build -f appa-runtime/Dockerfile -t appa-runtime:dev .
docker build -t appa-kagent-adk:dev integrations/kagent/appa-kagent-adk
docker build -t appa-demo-tools:dev integrations/kagent/demo
docker build -t appa-demo-mocks:dev integrations/kagent/demo/mocks
kind load docker-image appa-runtime:dev appa-kagent-adk:dev \
  appa-demo-tools:dev appa-demo-mocks:dev --name <cluster-name>
```

The reproducible composed install lives in
[`../../e2e/ci/install.sh`](../../e2e/ci/install.sh).

## Verify

[`tests/render-test.sh`](tests/render-test.sh) proves that the chart
renders no runtime-owned resource. The live UI and A2A matrices under
[`../../e2e`](../../e2e/) exercise the two-release composition and all
eighteen policy scenarios.
