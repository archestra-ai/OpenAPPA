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
    tag: 0.9.0 # x-release-please-version

  # Go declarative runtime image
  goAgentImage:
    registry: ghcr.io
    repository: archestra-ai/appa-kagent-adk-go
    tag: 0.9.0 # x-release-please-version
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
- [Helm](https://helm.sh/docs/intro/install/) (v3.8+)
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- An [OpenAI API key](https://platform.openai.com/api-keys) (or credentials for another [supported kagent provider](https://kagent.dev/docs/kagent/supported-providers/))

### 1. Install kagent with OpenAPPA

Install the kagent [CRDs and Helm chart](https://kagent.dev/docs/kagent/resources/helm/) configured with your OpenAI API key and the OpenAPPA runtime image:

```sh
export OPENAI_API_KEY="<your-api-key>"

# Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace

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
  --wait --timeout 10m \
  --set controller.agentImage.tag=0.9.0 # x-release-please-version
```

The kagent chart enables its stock sample agents by default. Setting `providers.openAI.apiKey` configures the default OpenAI provider for the cluster. The flags above disable the unused stock agents to keep the cluster lean. The explicit timeout makes Helm report a failed rollout instead of waiting without a visible deadline.

The parameters `providers.default=openAI` and `providers.openAI.apiKey` configure kagent's default provider and credentials. You can configure any LLM provider and model supported by kagent (such as Anthropic, Azure OpenAI, Google Vertex AI, AWS Bedrock, Ollama, or custom OpenAI-compatible gateways) by setting the corresponding chart parameters or referencing custom `ModelConfig` resources. See the [kagent Supported Providers documentation](https://kagent.dev/docs/kagent/supported-providers/) for details.

### 2. Deploy the shared OpenAPPA runtime

Deploy the OpenAPPA policy runtime using the official Helm chart from GHCR:

```sh
helm upgrade --install appa-runtime oci://ghcr.io/archestra-ai/charts/appa-runtime \
  --version 0.9.0 \
  --namespace appa --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --wait --timeout 10m
```

The runtime binds loopback at `127.0.0.1:18788` with an unprivileged NGINX relay sidecar exposing port `18789`. Gated agents connect via cluster DNS:

```text
http://appa-runtime.appa.svc.cluster.local:18789
```

- **Single replica with SQLite**: OpenAPPA evaluates algebraic monoids deterministically without distributed consensus overhead. SQLite stores the append-only trajectory audit log.
- **Persistence (`persistence.enabled=true`)**: Mounts a PersistentVolumeClaim at `/var/lib/appa`. It retains the trajectory log and provides writable storage for the `appa-guide` skill to download, verify, and refresh batteries.
- **Policy ConfigMap**: Mounts the `appa-policy` ConfigMap (key `appa.toml`) at `/etc/appa/appa.toml`. The runtime boots fail-closed until policy rules are configured.

If an install times out, inspect the cluster status:

```sh
kubectl get pods -n appa
kubectl get pods,agents -n kagent
kubectl get events -n kagent --sort-by='.lastTimestamp'
```

### 3. Deploy a protected agent

Deploy a declarative agent configured with `APPA_ENABLED: "true"` pointing to the shared runtime:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
metadata:
  name: sre-agent
  namespace: kagent
spec:
  type: Declarative
  description: "Site reliability agent protected by OpenAPPA."
  declarative:
    modelConfig: "default-model-config"
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: "http://appa-runtime.appa.svc.cluster.local:18789"
```

Save as `sre-agent.yaml` and apply:

```sh
kubectl apply -f sre-agent.yaml
```

The `modelConfig` field references the cluster's active `ModelConfig` resource (`default-model-config` by default). You can point this to any provider and model configured in your cluster per the [kagent Supported Providers documentation](https://kagent.dev/docs/kagent/supported-providers/).

Gated agents refuse to start without an active runtime connection (`fail-closed`).

### 4. Open the kagent dashboard

Forward the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:8080
```

Open [http://localhost:8901](http://localhost:8901) in your browser. Explore your agents under **Agents → sre-agent → Chat**, or deploy **appa-guide** below to manage policies.

## Protect existing agents

If you already run kagent in your cluster, you do not need to rebuild your agents or fork your code. You update the controller image and point your agents to `appa-runtime`.

### 1. Update the kagent controller image

Update your existing kagent Helm release to use the OpenAPPA runtime image:

```sh
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  -n kagent --reuse-values \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.9.0 \
  --set controller.goAgentImage.registry=ghcr.io \
  --set controller.goAgentImage.repository=archestra-ai/appa-kagent-adk-go \
  --set controller.goAgentImage.tag=0.9.0 # x-release-please-version
```

This image replaces the base container image for declarative agent pods. It stays inert until an agent enables `APPA_ENABLED: "true"`. Existing agents remain unaffected.

### 2. Wire existing agents to the runtime

To protect an existing [Agent](https://kagent.dev/docs/kagent/concepts/agents/), add `APPA_ENABLED` and `APPA_RUNTIME_URL` to `spec.declarative.deployment.env`:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
metadata:
  name: sre-agent
  namespace: default
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: "http://appa-runtime.appa.svc.cluster.local:18789"
```

Or patch an active agent resource directly with `kubectl`:

```sh
kubectl patch agent sre-agent -n default --type=merge -p '{
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

### 3. Confirm the gate

The kagent controller automatically rolls the agent deployment when the manifest changes. Check the rollout status and startup logs:

```sh
kubectl rollout status deployment/sre-agent -n default
kubectl logs -n default deployment/sre-agent | head -n 5
```

A gated agent logs confirmation during initialization:

```text
APPA_ENABLED is true. This agent runs gated by the OpenAPPA runtime at http://appa-runtime.appa.svc.cluster.local:18789
```

If the runtime is unreachable, tool calls stop fail-closed before execution.

## Manage integration with appa-guide

The `appa-guide` skill automates policy authoring, battery installation, and ongoing maintenance for your kagent cluster.

You interact with `appa-guide` through a dedicated declarative kagent agent. It uses Kubernetes tools from the kagent tool server to inspect your cluster, draft policies, and apply updates.

The `appa-guide` agent is itself gated by OpenAPPA. Manifest write operations (`k8s_apply_manifest`) require `attention = ["human-approval"]`. Every policy write raises kagent's native **Approve / Reject** card in the dashboard. No policy modification applies without explicit human approval.

### 1. Deploy the appa-guide agent

Apply this declarative manifest to create the `appa-guide` agent in your cluster:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
metadata:
  name: appa-guide
  namespace: kagent
spec:
  type: Declarative
  description: "Configure and maintain OpenAPPA policies, batteries, and runtime settings."
  skills:
    gitRefs:
      - url: "https://github.com/archestra-ai/OpenAPPA"
        ref: "main"
        path: "integrations/appa-guide"
        name: "appa-guide"
  declarative:
    systemMessage: |
      You configure OpenAPPA for this kagent cluster. When the operator says init or adjust,
      or asks to configure tools, batteries, or runtime policy, invoke the appa-guide skill
      and follow references/kagent.md. Work only through your k8s tools and read_file.
      Never modify policy without explicit operator approval in chat and through the kagent
      Approve card.
    modelConfig: "default-model-config" # matches the default kagent ModelConfig
    tools:
      - type: McpServer
        mcpServer:
          name: kagent-tools
          kind: RemoteMCPServer
          toolNames:
            - k8s_get_resources
            - k8s_get_resource_yaml
            - k8s_apply_manifest
            - k8s_execute_command
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: "http://appa-runtime.appa.svc.cluster.local:18789"
```

Save this file as `appa-guide.yaml` and apply it:

```sh
kubectl apply -f appa-guide.yaml -n kagent
```

### 2. Discover tools and author initial policy (`init`)

Open the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) at [http://localhost:8901](http://localhost:8901). Navigate to **Agents → appa-guide → Chat**, and type:

```text
init
```

The skill executes the following procedure:

1. **Cluster inventory**: Scans all `RemoteMCPServer` resources for discovered tools (`status.discoveredTools`). Scans all `Agent` resources for declared tools, memory tools, and delegations.
2. **Battery matching**: Calls `GET /batteries` on the runtime to identify pre-packaged battery matches.
3. **Wire translation**: Translates matched battery tool signatures to exact kagent wire names while keeping Annotators and Authorities intact.
4. **Root rule generation**: Generates root rules for tools not covered by batteries:
   - Confidential reads carry appropriate audience deltas.
   - External sinks require public audience.
   - State-changing actions require human approval (`attention = ["human-approval"]`).
   - Untrusted inputs mark data suspicious (`delta = { trust = "suspicious" }`).
5. **Plain English proposal**: Presents the proposed policy in chat with a clear summary:

```text
Operator: init
appa-guide: Discovered 8 tools and 3 agents in namespace 'kagent'.
            - Slack battery: Keeps Slack messages private and requires approval to post.
            - GitHub battery: Prevents private data leakage to public repositories.
            - Custom root rules: Added human approval for restart_deployment.
            - Ungated agents: 'analytics-worker' runs ungated.
            Writing policy to ConfigMap 'appa-policy' requires operator sign-off.
            Approve, or tell me what to change.
```

When you approve in chat, `appa-guide` executes `k8s_apply_manifest` to update the `appa-policy` ConfigMap. This action triggers the native kagent **Approve / Reject** confirmation card in the dashboard. Click **Approve**.

The agent confirms the ConfigMap sync and sends `POST /reload` to the runtime. The new policy activates immediately across the cluster without restarting agent pods.

### 3. Setup and refresh policy batteries

Batteries are maintained, composable policy bundles that supply contracts, annotators, and authority wiring for external systems.

During `init` or `adjust`, `appa-guide` matches your installed tools to available batteries and adds `include = ["batteries/<name>/appa.toml"]` to the policy.

When upstream batteries receive updates, you do not need to rebuild or restart the runtime container. Refresh batteries directly through `appa-guide`:

1. In chat with `appa-guide`, send:
   ```text
   refresh batteries
   ```
2. The skill confirms that persistence is enabled on the runtime PVC.
3. It runs `appa-refresh-batteries --check` via `k8s_execute_command` to discover the latest published semver release.
4. It displays the current and available version tags in chat and requests confirmation.
5. On your approval, `appa-guide` runs:
   ```sh
   appa-refresh-batteries --tag <version> ...
   ```
6. The command verifies the official release archive against its cryptographic `SHA256SUMS`, stages the release, tests the serving root configuration, and calls `POST /reload`.
7. If reload succeeds, it commits the release directory. If reload fails, it rolls back automatically to the previous layer.

The operator overlay (`/var/lib/appa/batteries`) remains untouched throughout the refresh.

### 4. Adjust policy rules (`adjust`)

When you add new MCP tools, connect new services, or want to alter existing permissions, open chat with `appa-guide` and state your goal:

```text
adjust require human approval before calling delete_namespace
```

The skill reads the current ConfigMap, verifies the existing rules with `appa describe`, drafts the minimal required change, and explains the outcome in plain language.

Once you confirm the proposal, `appa-guide` applies the update through `k8s_apply_manifest`, prompts for your confirmation card click, and reloads the runtime.

### 5. Audit and maintain integration health

The `appa-guide` skill continuously verifies cluster compliance during every interaction:

- **Detects ungated agents**: Reports any `Agent` where `APPA_ENABLED` is not `"true"` or where `APPA_RUNTIME_URL` does not match the shared runtime.
- **Identifies uninspected tools**: Warns when a `RemoteMCPServer` exists but has not completed tool discovery.
- **Read-only fallback**: If Kubernetes manifest write permissions are unavailable, `appa-guide` automatically falls back to read-only mode. It drafts the complete valid `appa.toml` directly into the chat for you to apply manually.

## Demonstration scenarios

The demo chart pre-seeds the kagent dashboard with interactive scenarios that verify each policy boundary.

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

Policies are declarative TOML files stored in the `appa-policy` ConfigMap or version control. This example policy excerpt governs cluster tools and human review:

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
