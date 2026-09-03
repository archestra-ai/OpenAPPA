---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Gate every declarative kagent agent on Kubernetes through a single container image setting.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents natively on Kubernetes. OpenAPPA adds deterministic security to kagent. It enforces data boundaries, stops data leaks, and requires human approvals before sensitive tools run.

You protect every [declarative agent](https://kagent.dev/docs/kagent/concepts/agents/) in your cluster with one Helm configuration value:

```yaml
# Helm values for the kagent controller
controller:
  agentImage:
    registry: ghcr.io
    repository: archestra-ai/appa-kagent-quickstart
    tag: 0.7.0
```

This setting requires no changes to agent manifests, no fork of kagent, and no fork of the Google Agent Development Kit (ADK).

## How it works

OpenAPPA runs inside the agent pod through the official Google ADK plugin API. Every [tool call](https://kagent.dev/docs/kagent/concepts/tools/) and [agent-to-agent delegation](https://kagent.dev/docs/kagent/examples/a2a-agents/) passes through the policy engine before execution.

:::fig-kagent:::

- **Enforcement occurs before execution**: A tool does not run if a policy requirement fails.
- **Fail-closed default**: If the policy runtime is unreachable, calls stop.
- **Runtime support**: Works with both Python (`appa-kagent-adk`) and Go (`appa-kagent-adk-go`) runtimes.

## Policy scope

The current integration enforces one cluster-wide union policy across all agents.

A single `appa.toml` policy file governs all [Agent](https://kagent.dev/docs/kagent/concepts/agents/) resources in the cluster. Individual per-agent policy overrides are not supported in this version.

## Quickstart

Follow this guide to deploy kagent with OpenAPPA and run your first protected agent in a test cluster.

### Prerequisites

Make sure you have installed:
- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) or an existing Kubernetes cluster
- [Helm](https://helm.sh/docs/intro/install/) (v3.8+)
- [kubectl](https://kubernetes.io/docs/tasks/tools/)
- An [OpenAI API key](https://platform.openai.com/account/api-keys)

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
  --set controller.agentImage.tag=0.7.0 --wait
```

### 2. Deploy the demo stack

Install the demo chart to create sample agents (`cluster-ops`, `log-analyst`) and 16 demonstration scenarios:

```sh
helm upgrade --install appa-kagent-demo ./integrations/kagent/demo/chart \
  -n kagent --set openai.apiKey="$OPENAI_API_KEY" --wait
```

### 3. Open the kagent dashboard

Forward the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:80
```

Open [http://localhost:8901](http://localhost:8901) in your browser.

## Protect an existing cluster

If you already run kagent in your cluster, you do not need to recreate your agents.

### 1. Update the kagent controller image

Update your existing Helm release to use the OpenAPPA quickstart image:

```sh
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  -n kagent --reuse-values \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.7.0
```

### 2. Restart agent deployments

Restart the agent deployments to load the OpenAPPA runtime container:

```sh
kubectl rollout restart deployment -n kagent -l app.kubernetes.io/managed-by=kagent
```

Your existing agents now route every tool call through OpenAPPA policy enforcement.

## Configure policy with appa-guide

The demo chart installs an `appa-guide` agent. It attaches the OpenAPPA guide skill through kagent's git-ref skills. It also provides the kagent tool server's Kubernetes tools. The shared runtime gates the guide agent's own tool calls. Open its chat and say `init`.

The canonical skill lives at `integrations/appa-guide`. Its `SKILL.md` routes to `references/claude-code.md` or `references/kagent.md`. kagent clones that directory directly. Claude packaging stages the same directory at its required plugin path. On kagent, the skill reads the policy ConfigMap. It inventories `RemoteMCPServer.status.discoveredTools` and each `Agent` tool declaration. It proposes contracts in plain English and waits for chat approval.

The skill applies the ConfigMap through `k8s_apply_manifest`. The fleet policy requires `attention = ["human-approval"]` for that call. Therefore, the kagent Approve / Reject card is the human decision. The skill then waits for the mounted policy to update and reloads the runtime. Any host with the same tools can run this skill. The pre-configured agent is only a convenience.

## 1. Try a blocked flow (Data leak prevention)

In the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/), inspect how OpenAPPA stops sensitive data from leaving the cluster.

1. The agent reads an internal Kubernetes secret:
   ```text
   read_secret(name: "db-credentials")
   ```
   OpenAPPA tags the session data with an audience restriction: `audience = ["ops"]`.

2. The agent attempts to publish that information to a public status channel:
   ```text
   post_status_update(message: "Database credentials updated...")
   ```

3. **OpenAPPA blocks the call before it runs.** The destination requires a `public` audience, but the data in context is restricted to `ops`. The tool never executes, and the model receives a clear explanation of the policy boundary.

## 2. Try human approval (HITL workflows)

OpenAPPA integrates with kagent's native [Human-in-the-Loop](https://kagent.dev/docs/kagent/examples/human-in-the-loop/) approval cards:

1. The agent proposes a destructive cluster action:
   ```text
   restart_deployment(name: "api-gateway")
   ```
   The policy requires explicit approval: `attention = ["human-approval"]`.

2. OpenAPPA intercepts the call and suspends the agent turn. An **Approve / Reject** confirmation card appears directly in the kagent dashboard.

3. **The operator decides**:
   - **Approve**: OpenAPPA authorizes the execution, records the approval, and allows the deployment to restart.
   - **Reject**: OpenAPPA cancels the request. The tool does not run.

## 3. Multi-agent delegation

When agents call other agents through [A2A (Agent-to-Agent)](https://kagent.dev/docs/kagent/examples/a2a-agents/) delegation, OpenAPPA isolates their execution contexts:

- **Inherited boundaries**: Child agents inherit the parent's data restrictions automatically.
- **Quarantine**: Untrusted operations (like inspecting raw pod logs) run inside the child agent. Only validated outputs flow back to the parent.
- **Explicit authorization**: Agents can only delegate to sub-agents explicitly listed in the policy. Unlisted agent spawns are blocked immediately.

## Policy example

Policies are declarative TOML files checked into version control. Here is the contract governing the demo agent:

```toml
# In-cluster secret read: restricts audience to ops
[[policy.tool]]
name = "read_secret"
delta = { audience = ["ops"] }

# Outward update: requires public audience
[[policy.tool]]
name = "post_status_update"
[policy.tool.requires]
audience = { contains = ["public"] }

# Production change: requires human operator sign-off
[[policy.tool]]
name = "restart_deployment"
[policy.tool.requires]
attention = ["human-approval"]

[[policy.authority]]
name = "oncall"
hint = "Ask the on-call lead through kagent approval card."
[policy.authority.permits]
attention = ["human-approval"]
```

## Where next

- [How it works](/how-it-works) — Core concepts, labels, and algebraic flow guarantees.
- [Policy contracts](/contracts) — Complete policy authoring and syntax guide.
- [kagent documentation](https://kagent.dev/docs/kagent/) — Official kagent guides and references.
- [Implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) — ADK callback lifecycle, Go/Python runtime architecture, and wire specs.
