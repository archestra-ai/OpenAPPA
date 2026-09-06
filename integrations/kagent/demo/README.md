# The kagent demo

The demo composes two Helm releases:

- [`charts/appa-runtime`](../../../charts/appa-runtime) owns the runtime,
  serving policy, persistence, and `appa-guide`.
- [`chart/`](chart/) owns only the demo Agents, tools, mock policy
  services, policy template, and seeded chats.

This boundary lets Quickstart, existing-Agent setup, and the demo use one
runtime installation. No chart competes for the same Kubernetes object.

## Install

Install kagent and appa first by following the public
[kagent Quickstart](https://openappa.com/kagent). Then install the fixture
chart:

```sh
APPA_VERSION=0.14.1 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n kagent \
  --set-string runtime.url=http://appa-runtime.appa.svc.cluster.local:18787 \
  --set-string modelConfig.name=default-model-config \
  --set-string runtime.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
kubectl wait -n kagent remotemcpserver/demo-tools \
  --for=jsonpath='{.status.discoveredTools[0].name}' \
  --timeout=2m
```

Open the dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Open `http://localhost:8080/agents/kagent/appa-guide/chat` and send
`init`. The guide verifies the demo release, reads its inert policy
template, matches the canned GitHub tools to the shipped GitHub battery,
and proposes the behavior. Approve the proposal in chat and then approve
the enforced confirmation card. A vouched runtime MCP operation validates,
publishes, and reloads the policy. A new `cluster-ops` chat uses it.

The dashboard contains sixteen seeded chats. They cover confidential
reads, suspicious ingress, sanitization, human approval, deterministic
Authorities, and delegated Agents. See [SCENARIOS.md](SCENARIOS.md).

## What the fixture chart owns

- `cluster-ops`, `log-analyst`, and intentionally undeclared
  `release-manager` Agents;
- optional Go twins of those Agents;
- the `demo-tools` Deployment, Service, and `RemoteMCPServer`;
- the `appa-demo-mocks` Deployment and Service;
- `ConfigMap/appa-kagent-demo-policy`, which is inert until approved;
- an idempotent seed Job for the sixteen captured chats.

Every Agent sets `APPA_ENABLED=true` and the configured
`APPA_RUNTIME_URL`. Parent and child therefore use the same runtime and
one Trajectory lineage.

The mock service implements `runbook-readers`, `release-window`,
`change-board`, and both demo sanitizers. The runtime invokes it through
fixed local command adapters. The mock's `/pending` and `/decide`
endpoints form the change board's out-of-band ruling channel.

The fixture chart owns no runtime Deployment, runtime Service, serving
policy, PersistentVolumeClaim, provider Secret, ModelConfig, or
`appa-guide` Agent.

`demo-tools` also exposes canned `mcp__github__get_file_contents` and
`mcp__github__issue_write`
calls for a public repository. They have no rules in the demo template.
`appa-guide` discovers their exact GitHub battery match and proposes the
include. Repository text then enters as suspicious, while trusted issue
text supplied by the operator can still publish.

## Tests

A deterministic variant runs the same tool and delegation paths with a
scripted model and local processes:

```sh
APPA_INTEGRATION=1 uv run --project integrations/kagent/appa-kagent-adk \
  --with "kagent-adk @ git+https://github.com/kagent-dev/kagent@v0.9.12#subdirectory=python/packages/kagent-adk" \
  --with "a2a-sdk>=0.3.23,<0.4" --with "google-adk==1.31.1" \
  --with "mcp>=1.25,<2" --with "pytest>=8" \
  pytest integrations/kagent/tests
```

The live two-chart installer and real-model matrices are under
[`../e2e`](../e2e/).
