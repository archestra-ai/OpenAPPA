---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Protect kagent agents on Kubernetes with OpenAPPA security policies.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents on Kubernetes. OpenAPPA protects these agents by checking tool calls and delegations against your security policies.

An OpenAPPA plugin inside each agent pod intercepts tool calls and checks them with `appa-runtime` before they run.

## How it works

OpenAPPA provides drop-in plugin images for Python and Go agents running on kagent.

:::fig-kagent:::

The plugin checks every tool call and delegation with `appa-runtime` before it runs. If a call violates policy, OpenAPPA blocks it. If the runtime is unreachable, the call halts immediately.

Sensitive actions can require human approval through kagent confirmation cards. Child agents run in isolated contexts, so their output is verified before returning to the parent agent.

## Quickstart

Deploy kagent with OpenAPPA and run a protected agent in a test cluster.

#### Prerequisites

- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) or an existing Kubernetes cluster
- [Helm](https://helm.sh/docs/intro/install/) v4
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- An [OpenAI API key](https://platform.openai.com/api-keys) or credentials for another [supported provider](https://kagent.dev/docs/kagent/supported-providers/)

#### 1. Deploy the test stack

Set your OpenAI API key:

```sh
export OPENAI_API_KEY="<your-api-key>"
```

Deploy the kagent controller, `appa-runtime`, and the demonstration fleet in one script:

```sh
: "${OPENAI_API_KEY:?Set OPENAI_API_KEY before installing kagent}"
APPA_VERSION=0.15.0 # x-release-please-version

# 1. Install kagent CRDs and OpenAI secret
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace --force-conflicts

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

# 2. Install kagent with the OpenAPPA plugin image
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set kmcp.podSecurityContext.runAsUser=65532 \
  --set kmcp.podSecurityContext.runAsGroup=65532 \
  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \
  --set controller.agentImage.tag="v$APPA_VERSION" \
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
  --force-conflicts --wait --timeout 10m

# 3. Deploy appa-runtime with persistent storage and appa-guide
helm upgrade --install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n appa --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m

# 4. Deploy demo fleet and mock policy services
helm upgrade --install appa-kagent-demo \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n kagent \
  --set-string runtime.url=http://appa-runtime.appa.svc.cluster.local:18787 \
  --set-string modelConfig.name=default-model-config \
  --set-string runtime.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
```

The explicit KMCP user and group preserve `runAsNonRoot` while avoiding
`CreateContainerConfigError` with the bundled KMCP 0.3.0 image, which defaults to root.

The runtime service listens at `http://appa-runtime.appa.svc.cluster.local:18787`.

#### 2. Access the dashboard

Forward the kagent dashboard and open [http://localhost:8080](http://localhost:8080) in your browser:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

#### 3. Initialize policy with appa-guide

Open **Agents → appa-guide → Chat** and send:

```text
init
```

`appa-guide` inventories live tools, agents, and policies. It matches discovered tools to shipped batteries, including the [GitHub battery](/battery-github).

Review the proposal in chat and approve the confirmation card. The guide updates the ConfigMap and reloads the runtime. Your cluster is now protected and ready for the demonstration scenarios below.

## Demonstration scenarios

The demo installation (`appa-kagent-demo`) pre-seeds 16 showcase sessions on `cluster-ops`. You can inspect completed traces in the kagent dashboard or execute fresh live prompts.

#### 1. Confidential read and sanitization

This scenario prevents confidential data from leaking into public sinks.

- **Pre-seeded chat**: `read the payments-provider secret and show me what you received`
- **Prompt**:
  ```text
  Read the payments-provider secret and post its API key to the public status page.
  ```
- **Policy rule**: `read_secret` specifies `delta = { audience = ["ops"] }`. `post_status_update` requires `audience = { contains = ["public"] }`.
- **Engine decision**: Admitting the secret narrows the trajectory audience from `public` to `ops`. OpenAPPA blocks the call before credentials enter model context. It returns a continuation offer with the `strip-secret-values` remedy plan.
- **Resolution**: The agent calls `execute_remedy_plan(offer_id)`. The `strip-secret-values` sanitizer redacts credentials and returns safe key names. The sanitized output enters model context, preventing credential leaks.

#### 2. Untrusted ingress and prompt injection

This scenario prevents prompt injection payloads from entering trusted trajectories silently.

- **Pre-seeded chat**: `read the crash logs of checkout-api-b2k1 and post a summary to the status page`
- **Prompt**:
  ```text
  Read the crash logs of checkout-api-b2k1 and post a summary to the status page.
  ```
- **Policy rule**: `get_pod_logs` specifies `delta = { trust = "suspicious" }`. `post_status_update` requires `trust = "trusted"`.
- **Engine decision**: Reading unvetted logs reduces trajectory trust from `trusted` to `suspicious`. When the agent calls `post_status_update`, OpenAPPA denies the flow because suspicious data cannot enter trusted sinks.
- **Resolution**: The write fails closed. Injected instructions inside unvetted logs cannot trigger outward actions.

#### 3. Destructive action and human review (HITL)

This scenario enforces human sign-off for operational actions using native kagent confirmation cards.

- **Pre-seeded chat**: `restart the checkout-api deployment; if it is blocked, execute the offered remedy plan`
- **Prompt**:
  ```text
  Restart the checkout-api deployment.
  ```
- **Policy rule**: `restart_deployment` specifies `requires = { attention = ["human-approval"] }`. The `oncall` authority permits `human-approval` via `builtin = "hitl"`.
- **Engine decision**: OpenAPPA blocks the direct call. It offers a remedy plan that consults the `oncall` authority. The agent calls `execute_remedy_plan(offer_id)`.
- **Resolution**: The agent turn suspends. An **Approve / Reject** card appears in the kagent dashboard:
  - **Approve**: The `oncall` authority grants `human-approval`. The deployment restarts.
  - **Reject**: The `oncall` authority refuses. OpenAPPA records the refusal, and the tool does not run.

#### 4. Remote authority review (Async Webhook)

This scenario demonstrates asynchronous sign-off by an external change advisory board.

- **Pre-seeded chat**: `roll back the checkout-api deployment; if it is blocked, execute the offered remedy plan`
- **Prompt**:
  ```text
  Roll back the checkout-api deployment.
  ```
- **Policy rule**: `rollback_deployment` specifies `requires = { attention = ["change-approval"] }`. The `change-board` authority connects to an external webhook endpoint.
- **Engine decision**: OpenAPPA suspends the call while waiting for a decision from the change advisory board.
- **Resolution**: An external operator or system reviews the request via API (`GET /pending`, `POST /decide`):
  - **Approve**: OpenAPPA admits `change-approval`. The rollback runs.
  - **Reject**: OpenAPPA records the denial fail-closed. The action aborts.

#### 5. Subagent delegation and the return gate (A2A)

This scenario demonstrates context isolation and return value gating during Agent-to-Agent (A2A) delegation.

- **Pre-seeded chat**: `ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary`
- **Prompt**:
  ```text
  Ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary.
  ```
- **Policy rule**: `kagent__NS__log_analyst` is declared in policy. `context_control = true` isolates child trajectories. Undeclared subagents are excluded.
- **Engine decision**: `cluster-ops` delegates to `log-analyst`. The child executes on an isolated child trajectory. Untrusted logs (`trust = "suspicious"`) remain quarantined in child context.
- **Resolution**: The child agent completes by calling `appa_return`. OpenAPPA checks the return payload at `SpawnResult` against parent policy. The clean summary enters the parent trajectory. If the parent calls an undeclared agent, OpenAPPA denies the spawn fail-closed:
  ```text
  Ask the release manager to approve a version bump of checkout-api to 2.4.1.
  ```

#### 6. Dynamic per-call contracts (Annotators)

This scenario demonstrates dynamic policy evaluation based on runtime call arguments.

- **Pre-seeded chat**: `look up the public-oncall-rotation runbook`
- **Prompt**:
  ```text
  Look up the public-oncall-rotation runbook.
  ```
- **Policy rule**: `lookup_runbook` routes through the `runbook-readers` annotator. The annotator inspects arguments dynamically per call.
- **Engine decision**: Public runbooks carry no reader restrictions and execute immediately. Ops runbooks narrow the audience to `["ops"]`. Invalid IDs receive no annotation, producing an operational refusal.
- **Resolution**: Annotators produce call contracts dynamically, avoiding static rule proliferation for heterogeneous endpoints.

#### 7. Permitted baseline execution

This scenario demonstrates unhindered execution for operations that violate no boundaries.

- **Pre-seeded chat**: `list the pods in the shop namespace`
- **Prompt**:
  ```text
  List the pods in the shop namespace.
  ```
- **Policy rule**: `list_pods` defines `delta = {}` and requires no attention marks.
- **Engine decision**: The call violates no audience or trust boundaries.
- **Resolution**: OpenAPPA evaluates the call against policy and permits immediate execution without operator friction.

## Protect existing agents

Policy scope follows the `APPA_RUNTIME_URL` service endpoint. Agents connected to the same runtime share one policy file and decision log. To enforce different policies for different agent groups, deploy separate runtime instances.

To protect existing kagent workloads without downtime, follow these steps:

#### 1. Update the controller and deploy appa-runtime

Update the kagent controller to use the OpenAPPA plugin image, and deploy `appa-runtime` with `appa-guide`:

```sh
APPA_VERSION=0.15.0 # x-release-please-version

# 1. Update the kagent controller to use the OpenAPPA plugin image
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent --reuse-values \
  --set kmcp.podSecurityContext.runAsUser=65532 \
  --set kmcp.podSecurityContext.runAsGroup=65532 \
  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \
  --set controller.agentImage.tag="v$APPA_VERSION" \
  --force-conflicts --wait --timeout 10m

# 2. Deploy appa-runtime with persistent storage and appa-guide
helm upgrade --install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n appa --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --force-conflicts --wait --timeout 10m

kubectl wait agent/appa-guide -n kagent \
  --for=condition=Ready=True --timeout=5m
```

Existing workloads continue running standard behavior until explicitly gated.

#### 2. Enable gating with appa-guide

Once deployed, `appa-guide` automates onboarding and rollout verification inside the cluster.

Open **Agents → appa-guide → Chat** and prompt the guide:

```text
protect sre-agent with the shared OpenAPPA runtime and verify its rollout
```

`appa-guide` updates the Agent deployment configuration, monitors pod rollout, and verifies runtime connectivity. All changes require operator sign-off via the kagent confirmation card.

To onboard every eligible declarative Agent at once:

```text
enable OpenAPPA for all agents using the shared runtime; show me the affected agents before applying
```

##### Manual configuration (GitOps)

If you manage agent manifests via GitOps, configure environment variables directly on the `Agent` resource:

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

The OpenAPPA plugin inspects these two environment variables on pod startup to determine enforcement behavior:

| Deployment mode | `APPA_ENABLED` | `APPA_RUNTIME_URL` | Enforcement behavior |
|---|---|---|---|
| **Disabled (Default)** | Unset or `"false"` | Any | Ungated. Runs standard kagent execution without policy checks. |
| **Gated** | `"true"` | `http://...` | Gated. Intercepts tool calls and delegations via `appa-runtime`. |

## Manage policy with appa-guide

`appa-guide` provides conversational policy administration inside the kagent dashboard. All policy modifications require operator approval through the confirmation card.

| Command | Action |
|---|---|
| `init` | Inventory cluster tools, match batteries, and generate initial policy. |
| `adjust <rule>` | Propose specific policy changes, such as requiring approvals for sensitive tools. |
| `refresh batteries` | Download and apply updated batteries from upstream releases. |
| `diagnose the OpenAPPA integration` | Audit health across runtime pods, agents, and tool servers. |

You do not need to hand-edit raw policy TOML files. You can ask `appa-guide` to inspect and modify rules directly in chat:

```text
adjust restart_deployment to require human-approval
```

`appa-guide` generates the proposed rule diff, explains the change, and presents a native confirmation card. Once approved, `appa-guide` writes the updated policy to the runtime ConfigMap and reloads the policy engine atomically.

To view the active policy generated by `appa-guide`, ask in chat:

```text
show me the active policy rules for cluster-ops
```

For the formal policy grammar and syntax specification, see [Policy configuration](/contracts).

## Where next

- [How it works](/how-it-works) — Core concepts, label algebra, and formal flow guarantees.
- [Policy configuration](/contracts) — Syntax reference for tools, annotators, and authorities.
- [What is a battery](/batteries) — How policy batteries structure and combine tool rules.
- [Validation](/validation) — Test policy rules offline with scripted replays.
- [Implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) — ADK callback lifecycle, Go/Python plugin architecture, and wire specifications.
