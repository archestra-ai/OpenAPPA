---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Gate declarative kagent agents through a shared OpenAPPA runtime.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents on Kubernetes. OpenAPPA gates enabled Agents through a shared `appa-runtime` Service.

Configure the appa plugin image in the kagent controller Helm values:

```yaml
# Helm values for the kagent controller
controller:
  # Python declarative runtime image
  agentImage:
    registry: europe-west1-docker.pkg.dev
    repository: friendly-path-465518-r6/appa-public/appa-kagent-adk
    tag: v0.14.0 # x-release-please-version
```

The plugin image replaces kagent's Python Agent image. It stays inert until an Agent sets both variables:

```yaml
apiVersion: kagent.dev/v1alpha2
kind: Agent
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: "http://appa-runtime.appa.svc.cluster.local:18787"
```

## How it works

The Python and Go plugin images run inside Agent pods through the official Google ADK plugin APIs. Every [tool call](https://kagent.dev/docs/kagent/concepts/tools/) and [agent-to-agent delegation](https://kagent.dev/docs/kagent/examples/a2a-agents/) crosses the remote policy engine before execution.

:::fig-kagent:::

- **Enforcement occurs before execution**: A tool does not run if a policy requirement fails.
- **Fail-closed default**: If the policy runtime is unreachable, calls stop.
- **Runtime support**: Works with both Python (`appa-kagent-adk`) and Go (`appa-kagent-adk-go`) runtimes.
- **Subagent return gate**: Delegated child agents stop through `appa_return`. OpenAPPA checks returned data at `SpawnResult` before parent context receives it.

On kagent 0.9.12, Go Agents derive `europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/golang-adk` from `controller.agentImage`. OpenAPPA publishes that alias on the `appa-kagent-adk-go` image digest. The stable chart has no `controller.goAgentImage` value.

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

### 1. Install kagent with appa plugin

Set your provider credential in the shell first. Replace the placeholder; do not run this line unchanged:

```sh
export OPENAI_API_KEY="<your-api-key>"
```

Then install kagent. This block does not modify `OPENAI_API_KEY` and stops before Helm when the variable is unset:

```sh
: "${OPENAI_API_KEY:?Set OPENAI_API_KEY before installing kagent}"
APPA_VERSION=0.14.0 # x-release-please-version

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

These flags disable kagent's stock Agents and unused bundled MCP servers. The provider key stays in a Kubernetes Secret instead of Helm release values. No gated Agent starts before the runtime exists. The controller, dashboard, default model, and tool server remain available.

The appa plugin preserves stock behavior when `APPA_ENABLED` is absent or `false`. Enabled Agents require a nonempty `APPA_RUNTIME_URL`. Gated callbacks fail closed if that endpoint is unreachable.

### 2. Install appa

Now install the runtime. kagent can reconcile `appa-guide` against the model and tool server from step 1:

```sh
APPA_VERSION=0.14.0 # x-release-please-version
helm upgrade --install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n appa --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
```

The runtime listens at `http://appa-runtime.appa.svc.cluster.local:18787`. The same release creates `appa-guide` in `kagent`.

### 3. Install the demo Agents

```sh
APPA_VERSION=0.14.0 # x-release-please-version
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

Helm `--wait` returns when the `demo-tools` pod is Ready. kagent discovers MCP tools after that. Wait until `demo-tools` reports at least one discovered tool before you send `init`.

This chart creates the protected `cluster-ops` fleet, demo tools, mock policy services, and sixteen seeded `cluster-ops` chats. `appa-guide` and every demo Agent use `gpt-5.6-luna` from the shared `default-model-config`. The adapter supplies `reasoning_effort: "none"`, which Luna requires for function tools on the chat completions API. Its canned public-GitHub tools intentionally match the shipped [GitHub battery](/battery-github). It does not create another runtime, policy owner, volume, provider credential, ModelConfig, or `appa-guide`.

### 4. Open the kagent dashboard

Forward the [kagent dashboard](https://kagent.dev/docs/kagent/observability/launch-ui/) to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Keep that command running. Open [http://localhost:8080](http://localhost:8080), then open **Agents → appa-guide → Chat**.

### 5. Initialize policy with appa-guide

Open **Agents → appa-guide → Chat** and send:

```text
init
```

As in Claude Code, `init` inventories every live `RemoteMCPServer`, Agent, policy, and available battery. It matches `mcp__github__get_file_contents` and `mcp__github__issue_write` to the GitHub battery. It also copies the demo fleet contracts that the bootstrap policy does not declare, including `read_secret`. The proposal explains that repository text becomes suspicious, public issue writes accept only trusted public data, and a secret read stays with the operations audience.

Review the complete proposal, reply with your approval, then approve the enforced kagent confirmation card. A typed, vouched runtime tool includes the GitHub battery, merges the missing demo contracts, preserves the complete root policy, updates the runtime-owned ConfigMap, reloads, and rolls back on failure. Installing the demo chart alone never changes serving policy.

### 6. Run and observe a protected flow

Open a new **cluster-ops** chat and send:

```text
Read acme/status-page RELEASE.md and use its text to file a public issue.
```

The battery marks repository content suspicious. The Agent can accept and inspect it, but the session then cannot send that data to `issue_write`, which requires trusted public input. In a fresh chat, trusted text supplied directly by the operator remains useful:

```text
File a public issue in acme/status-page titled "Docs" with body "Add installation steps."
```

That issue succeeds without human approval. The battery establishes a data boundary rather than disabling GitHub. Try the confidential-data scenario next:

```text
Read the payments-provider secret and post its API key to the public status page.
```

The secret read narrows the Value to the operations audience. The public post cannot receive it, so OpenAPPA offers a sanitized result instead of leaking the credential.

The demo chart seeds sixteen chats on **cluster-ops**. They include this secret case, human approval, prompt injection, a remote change board, and the two Agent-to-Agent cases below. Open a seeded chat, or send the next prompts in a new chat. `log-analyst` and `release-manager` are spawn targets for `cluster-ops`, not operator chats.

```text
Ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary.
```

`cluster-ops` delegates to `log-analyst`. The child reads the untrusted logs on its own Trajectory. The return gate checks the summary before it enters the parent. Injected instructions in the crash log do not reach the operator through the child.

```text
Ask the release manager to approve a version bump of checkout-api to 2.4.1.
```

`cluster-ops` lists `release-manager` as a tool. The policy does not name that Agent, so OpenAPPA denies the spawn.

Observe any of these decisions from another terminal:

```sh
kubectl logs -n appa deployment/appa-runtime -c runtime --tail=50
```

## Protect existing Agents

If you already run kagent, use this sequence to install OpenAPPA without interrupting existing Agents.

### 1. Install the OpenAPPA Agent image

The image preserves stock behavior unless an Agent enables OpenAPPA. On kagent 0.9.12, Go Agents use the published `golang-adk` alias at the same tag.

```sh
APPA_VERSION=0.14.0 # x-release-please-version
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent --reuse-values \
  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \
  --set controller.agentImage.tag="v$APPA_VERSION" \
  --force-conflicts --wait --timeout 10m
```

### 2. Install OpenAPPA

Install the policy runtime and wait for `appa-guide`:

```sh
APPA_VERSION=0.14.0 # x-release-please-version
helm upgrade --install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" \
  --namespace appa --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent \
  --force-conflicts \
  --wait --timeout 10m
kubectl wait agent/appa-guide -n kagent \
  --for=condition=Ready=True --timeout=5m
```

Agents reach this runtime at `http://appa-runtime.appa.svc.cluster.local:18787`. The runtime stores its trajectory log on the persistent volume. It reads policy from `appa-runtime-policy`. The release also installs `appa-guide` in `kagent`.

Use `appa-guide` for later Agent, runtime, and controller changes. Its write tools are gated by the adapter.

### 3. Protect your Agents

Send `appa-guide`:

```text
protect sre-agent with the shared OpenAPPA runtime and verify its rollout
```

To protect every eligible declarative Agent, send:

```text
enable OpenAPPA for all agents using the shared runtime; show me the affected agents before applying
```

The guide inventories the Agents and shows the exact changes for approval. It applies the complete observed spec and verifies the rollout.

If the guide is unavailable, edit the complete resource and preserve every existing environment entry:

```sh
kubectl edit agent sre-agent -n kagent
```

| Mode | `APPA_ENABLED` | `APPA_RUNTIME_URL` | Gating Behavior |
|---|---|---|---|
| **Disabled (Default)** | Unset or `"false"` | Any | Ungated. Runs stock kagent behavior without policy checks. |
| **Shared appa-runtime** | `"true"` | `http://...` | Gated. Connects to the cluster `appa-runtime` Service at `APPA_RUNTIME_URL`. |

Invalid values for `APPA_ENABLED` fail container startup immediately. `APPA_ENABLED=true` without a nonempty runtime URL also refuses startup.

A gated Agent confirms the runtime URL during initialization:

```text
APPA_ENABLED is true. This agent runs gated by the OpenAPPA runtime at http://appa-runtime.appa.svc.cluster.local:18787
```

If the runtime is unreachable, tool calls stop fail-closed before execution.

### 4. Configure and test the policy

Open **Agents → appa-guide → Chat** and send `init`. Review and approve the proposed behavior and the kagent confirmation card. The guide installs applicable batteries, reloads the runtime, and verifies the integration.

Run a protected action in a new Agent chat. Observe the resulting allow, block, or remedy in the chat and shared runtime log:

```sh
kubectl logs -n appa deployment/appa-runtime -c runtime --tail=50
```

## Manage integration with appa-guide

Only the shared runtime chart installs `appa-guide`. Step 2 installs it and waits until it is ready. Its two modes match the Claude Code experience: `init` creates the initial configuration, and `adjust` changes an existing configuration.

Run these interactions in order:

1. Send `init`. The guide inventories runtimes, Agents, RemoteMCPServers, tools, and current policy. It finds applicable batteries and proposes contracts for uncovered tools.
2. Review the complete behavior in plain English. Reply with your approval, then approve the kagent **Approve / Reject** card. A typed runtime operation validates, publishes, and reloads the policy atomically.
3. Send `refresh batteries` when you want a newer battery release. The guide verifies persistent storage and proposes one approved operation that verifies, stages, reloads, commits, or rolls back the release.
4. Send an `adjust` request for subsequent policy changes, such as `adjust require human approval before calling delete_namespace`.
5. Send `diagnose the OpenAPPA integration` to audit runtime, policy, battery, Agent, and tool-server health.

No policy write occurs without explicit approval.

The same chat is the ongoing control surface for OpenAPPA operations. Examples include `protect payments-agent`, `enable OpenAPPA for all agents`, `install the demo agents`, `upgrade the shared runtime`, `diagnose the cluster integration`, and `remove the demo deployment`. The guide inspects current state and presents the exact affected resources before requesting approval.

## Demonstration scenarios

Quickstart step 3 installs the scenario fleet against the same shared runtime. The fixture chart seeds sixteen chats on `cluster-ops`. Open a seeded chat, or send the matching prompt in a new chat.

The fixture release owns no runtime, serving policy, persistence, provider configuration, or `appa-guide`. It supplies an inert policy template that `appa-guide` verifies and merges only after approval.

The default dashboard contains four OpenAPPA Agents across the two releases. `appa-guide` manages policy, batteries, and integration lifecycle. `cluster-ops` is the primary demo Agent; every seeded chat and every prompt below runs there. `log-analyst` is its delegated child for gated-return scenarios. `release-manager` is intentionally omitted from policy to demonstrate denied delegation. Those two Agents are spawn targets, not operator chats.

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

Open a new `cluster-ops` chat, or the matching seeded chat, and send:

```text
Ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary.
```

`cluster-ops` calls `log-analyst` as a tool. When agents call other agents through [A2A (Agent-to-Agent)](https://kagent.dev/docs/kagent/examples/a2a-agents/) delegation, OpenAPPA isolates their execution contexts. Set `context_control = true` under `[policy.deployment]` to enable isolation.

- **Inherited boundaries**: Child agents inherit the parent's data restrictions automatically.
- **Quarantine**: Untrusted operations (like inspecting raw pod logs) run inside the child agent without affecting the parent context during execution.
- **Subagent return gate**: The child agent stops by calling the OpenAPPA-owned `appa_return` tool (`ChildEnd`). The parent's gate evaluates `SpawnResult` before outputs enter parent context. If return data would violate parent boundaries, OpenAPPA withholds the data and returns remedy offers.
- **Explicit authorization**: Agents can only delegate to sub-agents explicitly listed in the policy (`<namespace>__NS__<agent>`). Unlisted agent spawns are blocked fail-closed.

To see that denial, open the matching seeded chat or send:

```text
Ask the release manager to approve a version bump of checkout-api to 2.4.1.
```

`cluster-ops` lists `release-manager` as a tool. The policy does not name it, so the spawn is denied.

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
