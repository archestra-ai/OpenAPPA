---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Gate declarative kagent agents on Kubernetes through a single runtime image setting.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents natively on Kubernetes. OpenAPPA adds deterministic security to kagent. It enforces data boundaries, stops data leaks, and requires human approvals before sensitive tools run.

Configure the runtime image in the kagent controller Helm values:

```yaml
# Helm values for the kagent controller
controller:
  # Python declarative runtime image
  agentImage:
    registry: ghcr.io
    repository: archestra-ai/appa-kagent-quickstart
    tag: 0.10.0 # x-release-please-version

  # Go declarative runtime image
  goAgentImage:
    registry: ghcr.io
    repository: archestra-ai/appa-kagent-adk-go
    tag: 0.10.0 # x-release-please-version
```

The image replaces the default kagent runtime image. It stays inert until activated with `APPA_ENABLED: "true"`:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
```

## How it works

OpenAPPA runs inside the agent pod through the official Google ADK plugin API. Every [tool call](https://kagent.dev/docs/kagent/concepts/tools/) and [agent-to-agent delegation](https://kagent.dev/docs/kagent/examples/a2a-agents/) passes through the policy engine before execution.

:::fig-kagent:::

- **Enforcement occurs before execution**: A tool does not run if a policy requirement fails.
- **Fail-closed default**: If the policy runtime is unreachable, calls stop.
- **Runtime support**: Works with both Python (`appa-kagent-adk`) and Go (`appa-kagent-adk-go`) runtimes.
- **Subagent return gate**: Delegated child agents stop through `appa_return`. OpenAPPA checks returned data at `SpawnResult` before parent context receives it.

## Policy scope

Policy scope follows the runtime. A gated [Agent](https://kagent.dev/docs/kagent/concepts/agents/) enforces the policy of the `appa-runtime` named by its `APPA_RUNTIME_URL`.

Agents connecting to the same runtime share one `appa.toml` policy file and decision log. The current integration applies a single policy union across all connected agents in the cluster. Override rules per agent are not supported in this version. To enforce different policies for different agent groups, run separate `appa-runtime` deployments.

Cross-workload delegation requires a shared runtime deployment so parent and child pods reach the same policy engine.

## Quickstart

Follow this guide to deploy kagent with OpenAPPA and run your first protected agent in a test cluster.

### Prerequisites

Make sure you have installed:
- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) or an existing Kubernetes cluster
- [Helm](https://helm.sh/docs/intro/install/) v4
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- An [OpenAI API key](https://platform.openai.com/api-keys) (or credentials for another [supported kagent provider](https://kagent.dev/docs/kagent/supported-providers/))

### 1. Install kagent with OpenAPPA

Set your OpenAI API key in your terminal:

```sh
export OPENAI_API_KEY="<your-api-key>"
```

Then install the kagent [CRDs and Helm chart](https://kagent.dev/docs/kagent/resources/helm/) with the OpenAPPA runtime image:

```sh
# Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace --force-conflicts

# Install kagent controller with OpenAPPA runtime
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
  --set providers.default=openAI \
  --set-string providers.openAI.apiKey="$OPENAI_API_KEY" \
  --force-conflicts \
  --wait --timeout 10m \
  --set controller.agentImage.tag=0.10.0 # x-release-please-version
```

The kagent chart enables its stock sample agents by default. Setting `providers.openAI.apiKey` configures the default OpenAI provider for the cluster. The flags above disable the unused stock agents to keep the cluster lean. The explicit timeout makes Helm report a failed rollout instead of waiting without a visible deadline.

The parameters `providers.default=openAI` and `providers.openAI.apiKey` configure kagent's default provider and credentials. You can configure any LLM provider and model supported by kagent (such as Anthropic, Azure OpenAI, Google Vertex AI, AWS Bedrock, Ollama, or custom OpenAI-compatible gateways) by setting the corresponding chart parameters or referencing custom `ModelConfig` resources. See the [kagent Supported Providers documentation](https://kagent.dev/docs/kagent/supported-providers/) for details.

### 2. Install the OpenAPPA demo

Install the public demo chart with the API key you exported above:

```sh
APPA_VERSION=0.10.0 # x-release-please-version
helm upgrade --install appa-kagent-demo \
  "https://github.com/archestra-ai/OpenAPPA/releases/download/v${APPA_VERSION}/appa-kagent-demo-${APPA_VERSION}.tgz" \
  -n kagent \
  --set-string openai.apiKey="$OPENAI_API_KEY" \
  --set runtime.persistence.enabled=true \
  --set agents.go.enabled=false \
  --force-conflicts \
  --wait --timeout 10m
```

The chart installs a gated cluster-operations fleet, `appa-guide`, the demo tools, the OpenAPPA runtime, and 16 seeded showcase chats. It uses OpenAI's `gpt-4.1-mini` by default. You can select any compatible provider and model through the chart's `openai.*` and `llm.*` values.

Helm 4 uses server-side apply. `--force-conflicts` makes these chart values authoritative if the dashboard, `kubectl`, or `appa-guide` previously changed a chart-owned field such as `ModelConfig.spec.apiKeySecret`.

After `appa-guide` is installed, you can install or upgrade the demo without returning to Helm. Send it: `install or upgrade the OpenAPPA demo agents using the existing kagent model credentials`.

If the install times out, inspect the resource that did not become ready:

```sh
kubectl get pods,agents -n kagent
kubectl get events -n kagent --sort-by='.lastTimestamp'
```

### 3. Open the kagent dashboard

Forward the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Keep that command running while you use the dashboard. Open [http://localhost:8080](http://localhost:8080) in your browser. Open **Agents → cluster-ops → Chat** to run the seeded demo cases, or **Agents → appa-guide → Chat** to manage policies.

### 4. Initialize policy with appa-guide

Open **Agents → appa-guide → Chat** and send:

```text
init
```

As in Claude Code, `init` inventories the live tools, proposes an initial policy and matching batteries, and waits for your approval. Review its proposal, reply with your approval, and approve the kagent confirmation card. The guide applies the policy, reloads the runtime, and verifies the result.

### 5. Run and observe a protected flow

Open a new **cluster-ops** chat and send:

```text
Read the payments-provider secret and post the API key to the public status page.
```

OpenAPPA blocks the confidential read before the secret reaches model context and returns remedy offers. Follow the sanitizer remedy to continue with redacted data. Observe the recorded decisions from another terminal:

```sh
kubectl logs -n kagent deployment/appa-runtime -c runtime --tail=50
```

## Protect existing agents

If you already run kagent in your cluster, you do not need to rebuild your agents or fork your code. You update the controller image and point your agents to `appa-runtime`.

### 1. Update the kagent controller image

Update your existing kagent Helm release to use the OpenAPPA runtime image:

```sh
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent --reuse-values \
  --force-conflicts \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.9.0 \
  --set controller.goAgentImage.registry=ghcr.io \
  --set controller.goAgentImage.repository=archestra-ai/appa-kagent-adk-go \
  --set controller.goAgentImage.tag=0.10.0 # x-release-please-version
```

This image replaces the base container image for declarative agent pods. It stays inert until an agent enables `APPA_ENABLED: "true"`. Existing agents remain unaffected.

With `appa-guide` already available, send: `update the kagent controller to the current OpenAPPA agent images`.

### 2. Deploy the shared OpenAPPA runtime

Deploy one policy runtime for the agents you want to protect:

```sh
APPA_VERSION=0.10.0 # x-release-please-version
helm upgrade --install appa-runtime oci://ghcr.io/archestra-ai/charts/appa-runtime \
  --version "$APPA_VERSION" \
  --namespace appa --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --force-conflicts \
  --wait --timeout 10m
```

Agents reach this runtime at `http://appa-runtime.appa.svc.cluster.local:18789`. The runtime stores its trajectory log on the persistent volume and reads policy from the `appa-runtime-policy` ConfigMap. The same release installs `appa-guide` in the `kagent` namespace.

With `appa-guide` already available, send: `install or upgrade the shared OpenAPPA runtime with persistent battery storage`.

### 3. Confirm appa-guide

The runtime chart installed the configuring Agent against its bootstrap policy. Wait for kagent to accept it and finish its rollout:

```sh
kubectl wait agent/appa-guide -n kagent \
  --for=condition=Ready=True --timeout=5m
```

### 4. Wire existing agents to the runtime

Send `appa-guide`: `protect sre-agent with the shared OpenAPPA runtime and verify its rollout`.

To protect every eligible declarative Agent, send: `enable OpenAPPA for all agents using the shared runtime; show me the affected agents before applying`.

The guide inventories the Agents, proposes the exact patch, waits for approval, applies it through `k8s_patch_resource`, and verifies the rollout. If the guide is unavailable, use this bootstrap or recovery fallback:

```sh
kubectl patch agent sre-agent -n kagent --type=merge -p '{
  "spec": {
    "declarative": {
      "deployment": {
        "env": [
          {"name": "APPA_ENABLED", "value": "true"},
          {"name": "APPA_RUNTIME_URL", "value": "http://appa-runtime.appa.svc.cluster.local:18789"}
        ]
      }
    }
  }
}'
```

| Mode | `APPA_ENABLED` | `APPA_RUNTIME_URL` | Gating Behavior |
|---|---|---|---|
| **Disabled (Default)** | Unset or `"false"` | Any | Ungated. Runs stock kagent behavior without policy checks. |
| **Shared appa-runtime** | `"true"` | `http://...` | Gated. Connects to the cluster `appa-runtime` Service at `APPA_RUNTIME_URL`. |
| **Bundled appa-runtime** | `"true"` | Unset | Gated. Starts an embedded `appa-runtime` inside the pod reading `APPA_CONFIG_CONTENTS`. |

Invalid values for `APPA_ENABLED` fail container startup immediately. Gated agents refuse to run without a valid runtime connection.

### 5. Confirm the gate

The kagent controller automatically rolls the agent deployment when the manifest changes. Check the rollout status and startup logs:

```sh
kubectl rollout status deployment/sre-agent -n kagent
kubectl logs -n kagent deployment/sre-agent --tail=5
```

A gated agent logs confirmation during initialization:

```text
APPA_ENABLED is true. This agent runs gated by the OpenAPPA runtime at http://appa-runtime.appa.svc.cluster.local:18789
```

If the runtime is unreachable, tool calls stop fail-closed before execution.

### 6. Finish setup and exercise the policy

Open **Agents → appa-guide → Chat** and send `init`. Review and approve the proposed behavior and the kagent confirmation card. The guide installs applicable batteries, reloads the runtime, and verifies the integration.

Run that action in a new chat with the protected agent. Observe the resulting allow, block, or remedy in the chat and in the shared runtime log:

```sh
kubectl logs -n appa deployment/appa-runtime -c runtime --tail=50
```

## Manage integration with appa-guide

Both the demo chart and the shared runtime chart install `appa-guide`. For an existing-Agent integration, enable it in step 2 and confirm it in step 3. Its two modes match the Claude Code experience: `init` creates the initial configuration, and `adjust` changes an existing configuration.

Run these interactions in order:

1. Send `init`. The guide inventories runtimes, Agents, RemoteMCPServers, tools, and current policy. It finds applicable batteries and proposes contracts for uncovered tools.
2. Review the complete behavior in plain English. Reply with your approval, then approve the kagent **Approve / Reject** card. The guide writes the policy and reloads the runtime.
3. Send `refresh batteries` when you want to check for a newer battery release. The guide verifies persistent storage, presents the version change, and waits for approval before installing it.
4. Send an `adjust` request for subsequent policy changes, such as `adjust require human approval before calling delete_namespace`.
5. Send `diagnose the OpenAPPA integration` to audit runtime, policy, battery, Agent, and tool-server health.

No policy write occurs without explicit approval.

The same chat is the ongoing control surface for OpenAPPA operations. Examples include `protect payments-agent`, `enable OpenAPPA for all agents`, `install the demo agents`, `upgrade the shared runtime`, `diagnose the cluster integration`, and `remove the demo deployment`. The guide inspects current state and presents the exact affected resources before requesting approval.

## Demonstration scenarios

The demo chart pre-seeds the kagent dashboard with interactive scenarios that verify each policy boundary.

The default dashboard contains four OpenAPPA Agents. `appa-guide` manages policy, batteries, and integration lifecycle. `cluster-ops` is the primary demo Agent. `log-analyst` is its delegated child for gated-return scenarios. `release-manager` is intentionally omitted from policy to demonstrate denied delegation. The latter two are scenario fixtures, not general kagent defaults.

### 1. Confidential read and sanitization

Open the `cluster-ops` agent in the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/). Ask it to read the payments-provider secret and post the API key to the public status page. The demo chart includes this pre-configured scenario.

1. The agent proposes the confidential read:
   ```text
   read_secret(name: "payments-provider")
   ```
   `read_secret` carries `delta = { audience = ["ops"] }`. Admitting that secret would narrow the session's audience to ops readers alone.

2. **OpenAPPA denies the read.** OpenAPPA gates the flow that changes the label, preventing the secret from entering model context. The denial provides structured feedback with runnable continuation offers:
   ```text
   [appa] Blocked: this call cannot run yet.

   Why:
     - allowed readers would narrow: public -> 1 reader

   Continue:
     - Accept this change for the rest of this session:
       execute_remedy_plan(offer_id: "…")
     - Use sanitizer strip-secret-values's result:
       execute_remedy_plan(offer_id: "…")
   ```

3. **The agent stays productive.** In this chat, the agent invokes `execute_remedy_plan` to apply the `strip-secret-values` sanitizer. Redacted key names return to the model without credentials. If the agent accepts audience narrowing instead, subsequent calls to `post_status_update` (which require public audience) are blocked.

### 2. Destructive action and human review

OpenAPPA integrates with kagent's native [Human-in-the-Loop](https://kagent.dev/docs/kagent/examples/human-in-the-loop/) confirmation cards:

1. The agent proposes a destructive cluster action:
   ```text
   restart_deployment(name: "checkout-api")
   ```
   The policy requires explicit approval: `attention = ["human-approval"]`.

2. **OpenAPPA denies the direct call and offers a remedy plan** that consults the `oncall` authority. The agent executes the plan:
   ```text
   execute_remedy_plan(offer_id: "...")
   ```
   Because the plan requires human review, the agent turn suspends. An **Approve / Reject** card appears on the `execute_remedy_plan` call in the dashboard.

3. **The operator decides**:
   - **Approve**: `oncall` grants `human-approval`. OpenAPPA authorizes the execution, the agent re-proposes `restart_deployment`, and the deployment restarts.
   - **Reject**: `oncall` refuses. OpenAPPA records the refusal, and the tool does not execute.

### 3. Subagent delegation and the return gate

When agents call other agents through [A2A (Agent-to-Agent)](https://kagent.dev/docs/kagent/examples/a2a-agents/) delegation, OpenAPPA isolates their execution contexts. Set `context_control = true` under `[policy.deployment]` to enable isolation.

- **Inherited boundaries**: Child agents inherit the parent's data restrictions automatically.
- **Quarantine**: Untrusted operations (like inspecting raw pod logs) run inside the child agent without affecting the parent context during execution.
- **Subagent return gate**: The child agent stops by calling the OpenAPPA-owned `appa_return` tool (`ChildEnd`). The parent's gate evaluates `SpawnResult` before outputs enter parent context. If return data would violate parent boundaries, OpenAPPA withholds the data and returns remedy offers.
- **Explicit authorization**: Agents can only delegate to sub-agents explicitly listed in the policy (`<namespace>__NS__<agent>`). Unlisted agent spawns are blocked fail-closed.

## Policy example

Policies are declarative TOML files stored in the runtime policy ConfigMap or version control. This example policy excerpt governs cluster tools and human review:

```toml
# In-cluster secret read: results carry the ops audience
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

# Production change: requires human operator sign-off
[[policy.tool]]
name = "restart_deployment"
delta = {}

[policy.tool.requires]
trust = "trusted"
attention = ["human-approval"]

# Delegation: the log-analyst agent, called as a tool. kagent dispatches
# an agent tool as `<namespace>__NS__<agent>`, hyphens as underscores.
[[policy.tool]]
name = "kagent__NS__log_analyst"
delta = {}

# Children run on their own context and declare what returns carry
[policy.deployment]
context_control = true

# Human authority definition
[[policy.authority]]
name = "oncall"
hint = "Ask the on-call lead through the kagent approval flow."

[policy.authority.permits]
attention = ["human-approval"]

# Binds oncall authority to kagent dashboard confirmation cards
[externals.authorities.oncall]
builtin = "hitl"
```

The `builtin = "hitl"` binding connects the `oncall` authority to kagent's dashboard confirmation cards.

## Where next

- [How it works](/how-it-works) — Core concepts, labels, and algebraic flow guarantees.
- [Policy contracts](/contracts) — Complete policy authoring and syntax guide.
- [kagent documentation](https://kagent.dev/docs/kagent/) — Official kagent guides and references.
- [Implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) — ADK callback lifecycle, Go/Python runtime architecture, and wire specs.
