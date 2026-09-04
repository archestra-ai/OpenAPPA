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
    tag: 0.8.0 # x-release-please-version

  # Go declarative runtime image
  goAgentImage:
    registry: ghcr.io
    repository: archestra-ai/appa-kagent-adk-go
    tag: 0.8.0 # x-release-please-version
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
- **Runtime support**: Works with both Python (`appa-kagent-adk`) and Go (`appa-kagent-adk-go`) runtimes. Both plugins post the hook protocol's wire envelope (`protocol: 1`) directly to the runtime's `/hook` endpoint; the runtime serves them with `--adapter kagent` and prefixes every trajectory id with `kagent:`.
- **Subagent return gate**: Delegated child agents stop through `appa_return`. OpenAPPA checks returned data at `spawn_result` before parent context receives it.

## Policy scope

Policy scope follows the runtime. A gated [Agent](https://kagent.dev/docs/kagent/concepts/agents/) enforces the policy of the `appa-runtime` named by its `APPA_RUNTIME_URL`.

Agents connecting to the same runtime share one `appa.toml` policy file and decision log. To enforce different policies for different agent groups, run separate `appa-runtime` deployments.

Cross-workload delegation requires a shared runtime deployment so parent and child pods reach the same policy engine.

## Quickstart

Follow this guide to deploy kagent with OpenAPPA and run your first protected agent in a test cluster.

### Prerequisites

Make sure you have installed:
- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) or an existing Kubernetes cluster
- [Helm](https://helm.sh/docs/intro/install/) (v3.8+)
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- [git](https://git-scm.com/downloads)
- An [OpenAI API key](https://platform.openai.com/account/api-keys)

Clone the repository to run the demo chart:

```sh
git clone https://github.com/archestra-ai/OpenAPPA
cd OpenAPPA
```

### 1. Install kagent with OpenAPPA

Install the kagent [CRDs and Helm chart](https://kagent.dev/docs/kagent/resources/helm/) with the OpenAPPA runtime image:

```sh
# Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace

# Install kagent controller with OpenAPPA runtime
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.8.0 --wait # x-release-please-version
```

To build images from source, see [`integrations/kagent/README.md`](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/README.md).

### 2. Deploy the demo stack

Install the demo chart with your OpenRouter API key. It deploys sample agents (`cluster-ops`, `log-analyst`, `appa-guide`) and 16 demonstration scenarios:

```sh
helm upgrade --install appa-kagent-demo ./integrations/kagent/demo/chart \
  -n kagent --set openai.apiKey="$OPENROUTER_API_KEY" --wait
```

The demo chart sets `APPA_ENABLED=true` on all demo agents.

### 3. Open the kagent dashboard

Forward the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:8080
```

Open [http://localhost:8901](http://localhost:8901) in your browser.

## Protect an existing cluster

If you already run kagent in your cluster, you do not need to recreate your agents.

### 1. Update the kagent controller image

Update your existing Helm release to use the OpenAPPA runtime image:

```sh
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  -n kagent --reuse-values \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.8.0 # x-release-please-version
```

If you run Go agents, also set `controller.goAgentImage`:

```sh
  --set controller.goAgentImage.registry=ghcr.io \
  --set controller.goAgentImage.repository=archestra-ai/appa-kagent-adk-go \
  --set controller.goAgentImage.tag=0.8.0 # x-release-please-version
```

### 2. Write a policy that names your tools

The policy governs only declared tools. Unlisted tool calls stop fail-closed before execution.

Deploy an `appa-runtime` with an `appa.toml` declaring your tools. A wildcard entry (`name = "*"`) covers unlisted tools through an annotator.

The policy names each tool by its canonical tool id. The kagent adapter maps what the pod dispatches onto these ids:

| kagent dispatches | The policy names it |
|---|---|
| A tool of the `RemoteMCPServer` or `ToolServer` served at `<toolset>` | `mcp/<toolset>/<tool>` |
| An agent called as a tool | `agent/<namespace>/<agent>` |
| A kagent built-in, such as `ask_user`, `load_memory`, `save_memory`, `prefetch_memory`, or a skill tool | `host/kagent/<name>` |
| The entrypoint's code-execution and memory-persist gates | `host/kagent-gate/code_execution`, `host/kagent-gate/memory_persist` |
| The remedy tool | `appa/execute_remedy_plan`, which no policy declares |

The toolset is the first label of the server host in the rendered `params.url`, which is the resource name when the Service carries it. A gated agent must give each MCP entry an explicit `tools` list: without one the server decides the tool list at run time, and the plugin refuses to start.

The toolset name therefore constrains the endpoint that serves it. A gated agent may point an MCP entry only at the Kubernetes service forms of that same name — `<service>`, `<service>.<namespace>`, `<service>.<namespace>.svc`, `<service>.<namespace>.svc.cluster.local` — or at `localhost` or `127.0.0.1`. Any other host refuses the start, so the endpoint your contracts name is a cluster service address and not an arbitrary host. The check establishes no more than that: the toolset is the host's first label alone, so a service of the same name in another namespace, or an `ExternalName` Service that resolves an accepted address outside the cluster, carries the same policy identity `mcp/<toolset>/<tool>`. A cluster with a DNS domain other than `cluster.local` uses the shorter `<service>.<namespace>.svc` form.

Agent delegation requires an explicit tool entry under `agent/<namespace>/<agent>`. Wildcards do not cover delegation spawns. See [Policy contracts](/contracts#tool-names) for the grammar.

### 3. Gate the agents you choose

Add `APPA_ENABLED` and the runtime address to an agent's environment:

```yaml
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: http://appa-runtime.kagent.svc.cluster.local:18789
```

| Mode | `APPA_ENABLED` | `APPA_RUNTIME_URL` | Gating Behavior |
|---|---|---|---|
| **Disabled (Default)** | Unset or `"false"` | Any | Ungated. Runs without policy enforcement. |
| **Bundled appa-runtime** | `"true"` | Unset | Gated. Starts an embedded `appa-runtime` process on `127.0.0.1` inside the pod. |
| **Shared appa-runtime** | `"true"` | `http://...` | Gated. Connects to the `appa-runtime` Kubernetes Service at `APPA_RUNTIME_URL`. |

Apply this configuration with `kubectl edit agent <name> -n kagent` or update your GitOps manifests. Invalid values for `APPA_ENABLED` fail container startup immediately to prevent accidental ungated execution.

### 4. Confirm the gate

The kagent controller automatically rolls the agent deployment. Check pod startup logs to verify status:

```sh
kubectl logs -n kagent deployment/cluster-ops | head -n 5
```

A gated agent logs: `APPA_ENABLED is true. This agent runs gated by the OpenAPPA runtime at ...`.

## Configure policy with appa-guide

The demo chart installs an `appa-guide` agent that automates policy authoring.

1. Open the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) at [http://localhost:8901](http://localhost:8901).
2. Navigate to **Agents → appa-guide → Chat**.
3. Send `init` to start policy discovery.

The guide agent reads the cluster's `RemoteMCPServer` toolsets and `Agent` declarations. It drafts policy contracts in plain English and submits them in chat:

```text
Operator: init
appa-guide: Discovered 6 tools and 2 subagents in namespace 'kagent'.
            Drafted policy in appa.toml.
            Writing policy to ConfigMap 'appa-policy' requires operator sign-off.
```

When you agree, the agent calls `k8s_apply_manifest` to write the policy ConfigMap. The fleet policy requires `attention = ["human-approval"]` for manifest writes, displaying an **Approve / Reject** confirmation card in the dashboard. Click **Approve** to commit the policy. The runtime reloads the new contracts automatically.

## 1. Confidential read and sanitization

Open the `cluster-ops` agent in the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/). Ask it to read the payments-provider secret and post the API key to the public status page. The demo chart includes this pre-configured scenario.

1. The agent proposes the confidential read:
   ```text
   read_secret(name: "payments-provider")
   ```
   The contract `mcp/demo-tools/read_secret` carries `delta = { audience = ["ops"] }`. Admitting that secret would narrow the session's audience to ops readers alone.

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

3. **The agent stays productive.** In this chat, the agent invokes `execute_remedy_plan` to apply the `strip-secret-values` sanitizer. Redacted key names return to the model without credentials. If the agent accepts audience narrowing instead, subsequent calls to `mcp/demo-tools/post_status_update` (which require public audience) are blocked.

## 2. Destructive action and human review

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

## 3. Subagent delegation and the return gate

When agents call other agents through [A2A (Agent-to-Agent)](https://kagent.dev/docs/kagent/examples/a2a-agents/) delegation, OpenAPPA isolates their execution contexts. Set `context_control = true` under `[policy.deployment]` to enable isolation.

- **Inherited boundaries**: Child agents inherit the parent's data restrictions automatically.
- **Quarantine**: Untrusted operations (like inspecting raw pod logs) run inside the child agent without affecting the parent context during execution.
- **Subagent return gate**: The child agent stops by calling the OpenAPPA-owned `appa_return` tool (the `child_end` event). The parent's gate evaluates `spawn_result` before outputs enter parent context. If return data would violate parent boundaries, OpenAPPA withholds the data and returns remedy offers.
- **Explicit authorization**: Agents can only delegate to sub-agents explicitly listed in the policy (`agent/<namespace>/<agent>`). Unlisted agent spawns are blocked fail-closed.

## Policy example

Policies are declarative TOML files checked into version control. This excerpt from [`integrations/kagent/demo/chart/files/demo.appa.toml`](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/demo/chart/files/demo.appa.toml) governs the demo agents:

```toml
# In-cluster secret read from the `demo-tools` toolset: results carry the ops audience
[[policy.tool]]
name = "mcp/demo-tools/read_secret"
delta = { audience = ["ops"] }

# Outward update: requires public audience and trusted data
[[policy.tool]]
name = "mcp/demo-tools/post_status_update"
delta = {}

[policy.tool.requires]
trust = "trusted"
audience = { contains = ["public"] }

# Production change: requires human operator sign-off
[[policy.tool]]
name = "mcp/demo-tools/restart_deployment"
delta = {}

[policy.tool.requires]
trust = "trusted"
attention = ["human-approval"]

# Delegation: the log-analyst agent in the kagent namespace, called as a tool
[[policy.tool]]
name = "agent/kagent/log-analyst"
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
