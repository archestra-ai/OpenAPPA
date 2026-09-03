---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Gate every declarative kagent agent on Kubernetes through a single container image setting.
---

[kagent](https://github.com/kagent-dev/kagent) runs AI agents natively on Kubernetes. OpenAPPA adds deterministic security: it enforces data boundaries, prevents data exfiltration, and requires human approvals before sensitive tools execute.

A single container image setting (`controller.agentImage`) protects every declarative agent across your cluster. No agent prompt changes, no kagent forks, and no ADK modifications.

## How it works

OpenAPPA runs inside the agent pod via the official Google ADK plugin API. Every tool call and multi-agent delegation is evaluated against your policy before execution.

```text
                  ┌────────────────────────┐
                  │    kagent Operator     │
                  │  (watches Agent CRDs)  │
                  └───────────┬────────────┘
                              │ deploys agent pod
                              ▼
┌─ Agent Pod ──────────────────────────────────────┐
│                                                  │
│   Agent (LLM) ──▶ Proposed Tool Call             │
│                         │                        │
│                         ▼                        │
│                 AppaPluginKagent (ADK)           │
│                         │                        │
│                         ▼                        │
│                 OpenAPPA Runtime                 │
│                 (evaluates policy contracts)     │
│                         │                        │
│           ┌─────────────┴─────────────┐          │
│           ▼                           ▼          │
│      Allow Call                   Block Call     │
│     (tool runs)             (offers remedy /     │
│                              human approval)     │
└──────────────────────────────────────────────────┘
```

- **Policy enforcement happens before execution**: Tools never run if a policy requirement is unmet.
- **Fail-closed by default**: If the runtime is unreachable, calls are blocked.
- **Language support**: Supports both Python (`appa-kagent-adk`) and Go (`appa-kagent-adk-go`) runtimes.

## Quickstart

### 1. Install kagent with OpenAPPA

Install kagent using Helm, setting the agent image to the OpenAPPA quickstart image:

```sh
# Install kagent CRDs
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version 0.9.12 -n kagent --create-namespace

# Install kagent with OpenAPPA runtime image
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=archestra-ai/appa-kagent-quickstart \
  --set controller.agentImage.tag=0.7.0 --wait
```

### 2. Deploy the interactive demo

Deploy the pre-configured demo cluster with sample agents (`cluster-ops`, `log-analyst`) and 16 demonstration scenarios:

```sh
helm upgrade --install appa-kagent-demo ./integrations/kagent/demo/chart \
  -n kagent --set openai.apiKey="$OPENAI_API_KEY" --wait
```

Forward the kagent dashboard to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8901:80
```

Open [http://localhost:8901](http://localhost:8901) in your browser.

## 1. Try a blocked flow (Data leak prevention)

In the dashboard, inspect how OpenAPPA stops sensitive data from leaving the cluster.

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

OpenAPPA integrates with kagent's native approval cards:

1. The agent proposes a destructive cluster action:
   ```text
   restart_deployment(name: "api-gateway")
   ```
   The policy requires explicit approval: `attention = ["human-approval"]`.

2. OpenAPPA intercepts the call and suspends the agent's turn. An **Approve / Reject** confirmation card appears directly in the kagent dashboard.

3. **The operator decides**:
   - **Approve**: OpenAPPA authorizes the execution, records the approval, and allows the deployment to restart.
   - **Reject**: OpenAPPA cancels the request. The tool does not run.

## 3. Multi-agent delegation

When agents call other agents via the A2A protocol, OpenAPPA isolates their execution contexts:

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
- [Implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) — ADK callback lifecycle, Go/Python runtime architecture, and wire specs.
