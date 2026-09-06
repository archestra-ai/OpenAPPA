---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Gate declarative kagent agents through a shared OpenAPPA runtime.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents on Kubernetes. OpenAPPA gates declarative kagent agents through a shared `appa-runtime` service.

An OpenAPPA plugin runs inside agent pods via Google ADK plugin APIs. The plugin intercepts every tool call and agent-to-agent delegation. It submits proposed actions to OpenAPPA before execution proceeds.

## How it works

The OpenAPPA integration uses plugin images for Python (`appa-kagent-adk`) and Go (`appa-kagent-adk-go`). On kagent 0.9.12, Go agents derive `europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/golang-adk` from `controller.agentImage`. OpenAPPA publishes that alias on the `appa-kagent-adk-go` image digest.

:::fig-kagent:::

- **Enforcement before execution**: OpenAPPA evaluates policy before tools execute. Blocked tools do not run.
- **Fail-closed security**: If the runtime is unreachable, tool calls stop immediately.
- **Subagent return gate**: Child agents terminate through `appa_return`. OpenAPPA checks returned data at `SpawnResult` before the parent trajectory receives it.
- **Human-in-the-loop integration**: The runtime routes approval requests directly to native kagent confirmation cards.

## Policy scope

Policy scope follows the runtime service. A gated agent enforces the policy of the `appa-runtime` named in its `APPA_RUNTIME_URL`.

Agents connected to the same runtime share one `appa.toml` policy file and decision log. To enforce different policies for different agent groups, deploy separate `appa-runtime` instances.

Cross-workload delegations require a shared runtime deployment. Parent and child pods must reach the same policy engine to maintain trajectory lineage.

## Quickstart

Deploy kagent with OpenAPPA and run a protected agent in a test cluster.

### Prerequisites

- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) or an existing Kubernetes cluster
- [Helm](https://helm.sh/docs/intro/install/) v4
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- An [OpenAI API key](https://platform.openai.com/api-keys) or credentials for another [supported provider](https://kagent.dev/docs/kagent/supported-providers/)

### 1. Install kagent with the OpenAPPA plugin

Set your OpenAI API key:

```sh
export OPENAI_API_KEY="<your-api-key>"
```

Install the kagent CRDs, create the provider secret, and install the kagent controller:

```sh
: "${OPENAI_API_KEY:?Set OPENAI_API_KEY before installing kagent}"
APPA_VERSION=0.14.1 # x-release-please-version

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

helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
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
```

The plugin image replaces kagent's default agent image. It preserves standard behavior until an agent explicitly enables OpenAPPA.

### 2. Deploy the OpenAPPA runtime

Deploy `appa-runtime` with persistent storage and `appa-guide` enabled:

```sh
APPA_VERSION=0.14.1 # x-release-please-version
helm upgrade --install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n appa --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
```

The runtime service listens at `http://appa-runtime.appa.svc.cluster.local:18787`.

### 3. Deploy the demo agents

Install the demo fleet and mock services:

```sh
APPA_VERSION=0.14.1 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n kagent \
  --set-string runtime.url=http://appa-runtime.appa.svc.cluster.local:18787 \
  --set-string modelConfig.name=default-model-config \
  --set-string runtime.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
```

This chart deploys the protected `cluster-ops` fleet, demo tools, and mock policy services.

### 4. Access the dashboard

Forward the kagent dashboard to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Open [http://localhost:8080](http://localhost:8080) in your browser.

### 5. Initialize policy with appa-guide

Open **Agents → appa-guide → Chat** and send:

```text
init
```

`appa-guide` inventories live tools, agents, and policies. It matches discovered tools to shipped batteries, including the [GitHub battery](/battery-github).

Review the proposal in chat. Reply with your approval, then approve the kagent confirmation card. The guide atomically writes the ConfigMap and reloads the runtime.

### 6. Verify policy enforcement

Open a chat with **cluster-ops** and test a protected tool flow:

```text
Read acme/status-page RELEASE.md and use its text to file a public issue.
```

The GitHub battery marks repository text as suspicious. OpenAPPA allows the read but blocks the subsequent public issue write because `issue_write` requires trusted data.

To view runtime decisions in real time, inspect the runtime logs:

```sh
kubectl logs -n appa deployment/appa-runtime -c runtime -f
```

## Demonstration scenarios

The demo installation includes pre-configured scenarios on the `cluster-ops` agent.

### 1. Confidential read and sanitization

This scenario demonstrates data exfiltration prevention and automatic remedy plans.

Ask `cluster-ops` to read a secret and publish it:

```text
Read the payments-provider secret and post its API key to the public status page.
```

1. **Confidential read proposed:**
   The agent calls `read_secret(name: "payments-provider")`. The tool contract defines `delta = { audience = ["ops"] }`. Admitting the secret would narrow the trajectory audience from public to `ops`.

2. **OpenAPPA blocks the read:**
   OpenAPPA blocks the call before secret data enters model context. It returns a continuation offer with a remedy plan:
   ```text
   [appa] Blocked: this call cannot run yet.

   Why:
     - allowed readers would narrow: public -> 1 reader

   Continue:
     - Use sanitizer strip-secret-values's result:
       execute_remedy_plan(offer_id: "…")
   ```

3. **Remedy plan executes:**
   The agent calls `execute_remedy_plan` using the offer ID. The `strip-secret-values` sanitizer redacts credentials and returns safe key names. The sanitized output enters the model context, and the secret is not leaked.

### 2. Destructive action and human review

This scenario demonstrates human-in-the-loop policy enforcement using native kagent confirmation cards.

Ask `cluster-ops` to restart a production deployment:

```text
Restart the checkout-api deployment.
```

1. **Destructive action proposed:**
   The agent calls `restart_deployment(name: "checkout-api")`. The policy requires operator approval: `requires = { attention = ["human-approval"] }`.

2. **OpenAPPA blocks and offers review:**
   OpenAPPA blocks the direct call. It offers a remedy plan that consults the `oncall` authority. The agent calls `execute_remedy_plan(offer_id: "…")`.

3. **Operator decides:**
   The agent turn suspends. An **Approve / Reject** card appears in the kagent dashboard:
   - **Approve**: The `oncall` authority grants `human-approval`. The deployment restarts.
   - **Reject**: The `oncall` authority refuses. OpenAPPA records the refusal, and the tool does not run.

### 3. Subagent delegation and the return gate

This scenario demonstrates context isolation and return value gating during Agent-to-Agent (A2A) delegation.

Ask `cluster-ops` to delegate log analysis to `log-analyst`:

```text
Ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary.
```

1. **Context isolation:**
   `cluster-ops` delegates to `log-analyst`. With `context_control = true`, the child agent executes on an isolated child trajectory. Untrusted logs (`trust = "suspicious"`) remain quarantined in the child context.

2. **Return gate validation:**
   The child agent completes by calling `appa_return`. OpenAPPA evaluates the return payload against parent policy at `SpawnResult`. The clean summary enters the parent trajectory, while untrusted prompt injections inside raw logs cannot reach the parent.

3. **Unauthorized delegation blocked:**
   If `cluster-ops` attempts to delegate to an undeclared agent:
   ```text
   Ask the release manager to approve a version bump of checkout-api to 2.4.1.
   ```
   The policy does not declare `release-manager`. OpenAPPA denies the spawn fail-closed.

## Protect existing agents

To protect existing kagent workloads without downtime, follow these steps.

### 1. Update the controller image

Update the kagent controller to use the OpenAPPA agent image. Existing agents continue running standard behavior:

```sh
APPA_VERSION=0.14.1 # x-release-please-version
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent --reuse-values \
  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \
  --set controller.agentImage.tag="v$APPA_VERSION" \
  --force-conflicts --wait --timeout 10m
```

### 2. Deploy appa-runtime

Deploy the runtime service in the `appa` namespace:

```sh
APPA_VERSION=0.14.1 # x-release-please-version
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

### 3. Enable gating on an Agent

Enable OpenAPPA by setting environment variables on the target `Agent` resource:

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

| Mode | `APPA_ENABLED` | `APPA_RUNTIME_URL` | Behavior |
|---|---|---|---|
| **Disabled (Default)** | Unset or `"false"` | Any | Ungated. Runs standard kagent execution without policy checks. |
| **Gated** | `"true"` | `http://...` | Gated. Tool calls and delegations cross `appa-runtime` before execution. |

You can also prompt `appa-guide` to automate onboarding:

```text
protect sre-agent with the shared OpenAPPA runtime and verify its rollout
```

To protect every eligible declarative Agent, send:

```text
enable OpenAPPA for all agents using the shared runtime; show me the affected agents before applying
```

## Manage policy with appa-guide

`appa-guide` provides conversational policy administration inside the kagent dashboard. All policy modifications require operator approval through the confirmation card.

| Command | Action |
|---|---|
| `init` | Inventory cluster tools, match batteries, and generate initial policy. |
| `adjust <rule>` | Propose specific policy changes, such as requiring approvals for sensitive tools. |
| `refresh batteries` | Download and apply updated batteries from upstream releases. |
| `diagnose the OpenAPPA integration` | Audit health across runtime pods, agents, and tool servers. |

## Policy example

Policies are written in declarative TOML. The following configuration illustrates tool contracts, requirements, subagent delegation, and human authorities:

```toml
# Read secret: result narrows trajectory audience to ops
[[policy.tool]]
name = "read_secret"
delta = { audience = ["ops"] }

# Outward update: requires public audience and trusted data
[[policy.tool]]
name = "post_status_update"
delta = {}

[policy.tool.requires]
trust = "trusted"
audience = { contains = ["public"] }

# Production change: requires human operator approval
[[policy.tool]]
name = "restart_deployment"
delta = {}

[policy.tool.requires]
trust = "trusted"
attention = ["human-approval"]

# Subagent delegation: log-analyst agent called as a tool
# kagent formats agent tools as <namespace>__NS__<agent_name>
[[policy.tool]]
name = "kagent__NS__log_analyst"
delta = {}

# Isolate subagent execution trajectories
[policy.deployment]
context_control = true

# Human authority definition
[[policy.authority]]
name = "oncall"
hint = "Ask the on-call lead through the kagent approval flow."

[policy.authority.permits]
attention = ["human-approval"]

# Bind oncall authority to native kagent confirmation cards
[externals.authorities.oncall]
builtin = "hitl"
```

## Where next

- [How it works](/how-it-works) — Core concepts, label algebra, and formal flow guarantees.
- [Policy contracts](/contracts) — Syntax reference for tools, annotators, and authorities.
- [What is a battery](/batteries) — How policy batteries structure and combine tool rules.
- [Validation](/validation) — Test policy rules offline with scripted replays.
- [Implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) — ADK callback lifecycle, Go/Python plugin architecture, and wire specifications.
