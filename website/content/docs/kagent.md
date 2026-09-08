---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Protect kagent declarative Python Agents with OpenAPPA policy.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents natively on [Kubernetes](https://kubernetes.io/docs/home/). OpenAPPA adds flow control to these [Agents](https://kagent.dev/docs/kagent/concepts/agents/), checking tool calls, sub-agents, and retrieved data against deterministic policy before any action runs.

## How it works

:::fig-kagent:::

OpenAPPA kagent runtime ADK plugin intercepts tool calls and subagents before they run, as well as their results:

- **Enforce policy:** Blocked actions stop immediately. When human review is required, kagent negotiates human approval in the chat or via A2A.
- **Isolate subagents:** Outputs from subagents are checked against policy before the parent agent is allowed to see them.
- **Shared runtime:** Agents connect to an `appa-runtime` service that evaluates policy and records audit logs. New policies apply automatically to new chats.

## Quickstart

The quickstart installs the kagent controller and agent runtime with the OpenAPPA plugin, the `appa-runtime` service with a pre-configured policy, and demo agents with tools for the showcase scenarios.

*(Already have kagent running? Skip to [Protect existing agents](#protect-existing-agents).)*

#### Prerequisites

- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) (or any local [Kubernetes cluster](https://kubernetes.io/docs/setup/)), [Helm](https://helm.sh/docs/intro/install/) v4, and [kubectl](https://kubernetes.io/docs/tasks/tools/).
- An [OpenAI API key](https://platform.openai.com/api-keys) (other [supported providers](https://kagent.dev/docs/kagent/supported-providers/) need their own kagent provider settings, Secret, and [ModelConfig](https://kagent.dev/docs/kagent/resources/api-ref/#modelconfig)).

#### 1. Deploy the demo stack

Make sure your `OPENAI_API_KEY` is exported:

```sh
export OPENAI_API_KEY="your-api-key"
```

Then deploy the stack:

```sh
APPA_VERSION=0.15.0 # x-release-please-version
KAGENT_VERSION=0.9.12
KAGENT_NAMESPACE=kagent

kubectl config current-context

helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version "$KAGENT_VERSION" -n "$KAGENT_NAMESPACE" --create-namespace \
  --force-conflicts --wait --timeout 10m

kubectl create secret generic kagent-openai -n "$KAGENT_NAMESPACE" \
  --from-literal=OPENAI_API_KEY="$OPENAI_API_KEY" \
  --dry-run=client -o yaml | kubectl apply -f -

helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version "$KAGENT_VERSION" -n "$KAGENT_NAMESPACE" \
  --set registry=ghcr.io \
  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \
  --set-string controller.agentImage.tag="v$APPA_VERSION" \
  --set providers.default=openAI \
  --set-string providers.openAI.apiKeySecretRef=kagent-openai \
  --set-string providers.openAI.apiKeySecretKey=OPENAI_API_KEY \
  --set-string providers.openAI.model=gpt-5.6-terra \
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

helm upgrade --install appa-kagent-demo \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n "$KAGENT_NAMESPACE" \
  --set-string runtime.url="http://appa-runtime.$KAGENT_NAMESPACE.svc.cluster.local:18787" \
  --set-string modelConfig.name=default-model-config \
  --set-string runtime.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m

helm upgrade --install appa-runtime \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n "$KAGENT_NAMESPACE" \
  --set persistence.enabled=false \
  --set config.existingConfigMap=appa-kagent-demo-policy \
  --force-conflicts --wait --timeout 10m

kubectl rollout status deployment/appa-runtime -n "$KAGENT_NAMESPACE" --timeout=5m
kubectl rollout status deployment/cluster-ops -n "$KAGENT_NAMESPACE" --timeout=5m
kubectl wait remotemcpserver/demo-tools -n "$KAGENT_NAMESPACE" \
  --for=jsonpath='{.status.discoveredTools[0].name}' --timeout=2m
```

To clean up the demo later, see [Uninstall](#uninstall).

#### 2. Open the dashboard

Forward the kagent dashboard to your machine:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Open [http://localhost:8080](http://localhost:8080), select **Agents**, **`cluster-ops`**, and **Chat** to test the demonstration scenarios below.

## Demonstration scenarios

In the kagent dashboard ([http://localhost:8080](http://localhost:8080)), select **Agents**, **`cluster-ops`**, and **Chat**. You can inspect pre-recorded runs from the chat history dropdown, or start a new chat to test the prompts live:

#### 1. Confidential read

```text
Read the payments-provider secret and post its API key to the public status page.
```

OpenAPPA blocks the public write unless passed through an authorized sanitizer that strips credential values.

#### 2. Untrusted ingress

```text
Read the crash logs of checkout-api-b2k1 and post a summary to the status page.
```

OpenAPPA blocks posting raw crash logs until an instruction-stripping sanitizer neutralizes any potential prompt injections.

#### 3. Human review

```text
Restart the checkout-api deployment.
```

Restarting a deployment is a sensitive action. The `oncall` authority requires explicit approval through the native confirmation card before the restart proceeds.

#### 4. Remote change board

```text
Rollback the checkout-api deployment.
```

The policy routes this request to an external change board service.

Forward the change board port in another terminal:

```sh
kubectl port-forward -n kagent svc/appa-demo-mocks 8081:8081
```

Inspect pending requests:

```sh
curl http://localhost:8081/pending
```

Approve the change:

```sh
ID=$(curl -s http://localhost:8081/pending | grep -o '"id":"[^"]*' | head -1 | cut -d'"' -f4)
curl -X POST http://localhost:8081/decide \
  -H "Content-Type: application/json" \
  -d "{\"id\": \"$ID\", \"ruling\": \"approve\"}"
```

#### 5. Subagents

```text
Ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary.
```

The subagent runs in an isolated session. Its output is checked against policy before the parent agent can see it, and unauthorized subagents (like `release-manager`) are blocked upfront.

#### 6. Dynamic input rules

```text
Look up the public-oncall-rotation runbook.
```

Instead of static tool permissions, OpenAPPA inspects call arguments dynamically: reading a `public-*` runbook allows data to be shared freely, while reading an internal `ops-*` runbook restricts the retrieved information to internal operations.

#### 7. Permitted read

```text
List the pods in the shop namespace.
```

A standard read operation with no sensitive data or external risks flows through without interruption.

## Protect existing agents

If you already have kagent running with your own agents, use `appa-guide` to configure policy and protect them conversationally.

#### 1. Deploy the adapter image and runtime

Update the kagent controller to use the `appa-kagent-adk` adapter image, and deploy `appa-runtime` with persistence enabled for audit logs and battery updates (requires a `ReadWriteOnce` [StorageClass](https://kubernetes.io/docs/concepts/storage/storage-classes/)):

```sh
APPA_VERSION=0.15.0 # x-release-please-version
KAGENT_VERSION=0.9.12
KAGENT_NAMESPACE=kagent
RUNTIME_NAMESPACE=appa

helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version "$KAGENT_VERSION" -n "$KAGENT_NAMESPACE" --reuse-values \
  --set registry=ghcr.io \
  --set controller.agentImage.registry=europe-west1-docker.pkg.dev \
  --set controller.agentImage.repository=friendly-path-465518-r6/appa-public/appa-kagent-adk \
  --set-string controller.agentImage.tag="v$APPA_VERSION" \
  --force-conflicts --wait --timeout 10m

helm upgrade --install appa-runtime \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n "$RUNTIME_NAMESPACE" --create-namespace \
  --set persistence.enabled=true \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace="$KAGENT_NAMESPACE" \
  --set-string appaGuide.modelConfig=default-model-config \
  --set-string appaGuide.toolServer.name=kagent-tool-server \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m

kubectl rollout status deployment/appa-runtime -n "$RUNTIME_NAMESPACE" --timeout=5m
kubectl wait agent/appa-guide -n "$KAGENT_NAMESPACE" \
  --for=condition=Ready=True --timeout=5m
```

To remove the adapter and runtime later, see [Restore stock images and remove OpenAPPA](#restore-stock-images-and-remove-openappa).

#### 2. Initialize policy and batteries with appa-guide

Forward the kagent dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Open [http://localhost:8080](http://localhost:8080), select **Agents**, **appa-guide**, and **Chat**, then send:

```text
init
```

`appa-guide` inspects the tools and MCP servers discovered in your cluster, matches relevant [batteries](/batteries) (such as GitHub or Slack), and drafts a tailored policy configuration.

To activate the policy:
1. Approve the proposal in chat (for example: `Approve the proposed policy`).
2. An **Approve / Reject** confirmation card will appear in the dashboard. Click **Approve**.

Once approved, the runtime activates and serves the new policy immediately.

#### 3. Protect your agents with appa-guide

In the same chat with `appa-guide`, ask it to protect any of your existing agents:

```text
protect <your-agent-name>
```

You can also protect every declarative agent at once:

```text
protect all agents
```

`appa-guide` inspects the agent manifest, proposes setting `APPA_ENABLED=true` and `APPA_RUNTIME_URL="http://appa-runtime.appa.svc.cluster.local:18787"`, and presents the exact manifest diff for confirmation.

Reply with approval in chat, then click **Approve** on the native confirmation card. `appa-guide` applies the manifest and verifies the pod rollout.

#### Manual configuration (GitOps)

If you manage your agents via GitOps manifests rather than `appa-guide`, add the environment variables directly to the Agent resource:

```yaml
spec:
  declarative:
    deployment:
      env:
        - name: APPA_ENABLED
          value: "true"
        - name: APPA_RUNTIME_URL
          value: "http://appa-runtime.appa.svc.cluster.local:18787"
```

When `APPA_ENABLED` is true, all tool calls route through OpenAPPA. If the runtime is unreachable, the agent fails closed to prevent unauthorized actions. Setting `APPA_ENABLED=false` or leaving it unset runs the agent without gating.

#### Multiple policies across agent groups

Agents that point to the same `APPA_RUNTIME_URL` share a policy deployment. If you need distinct policies for different teams or agent groups, deploy separate `appa-runtime` Helm releases (for example, `appa-runtime-finance`, `appa-runtime-ops`) in their respective namespaces, each with its own service URL.

## Manage policy with appa-guide

Use `appa-guide` in the kagent chat to inspect and modify policies conversationally instead of editing raw ConfigMaps:

- **`init`**: Scans available tools and generates a tailored policy configuration.
- **`adjust <rule>`**: Modifies specific tool contracts, trust levels, or audience boundaries.
- **`refresh batteries`**: Updates included battery definitions (requires persistence).
- **`diagnose the OpenAPPA integration`**: Runs read-only health checks on connectivity and configuration.

To prevent unintended modifications, policy changes require two confirmations: first in chat, then on the native confirmation card. Once confirmed, the runtime validates and applies the configuration immediately. New policies take effect on subsequent chats.

## Troubleshooting

- **Tools do not appear in kagent:** Check the status of your tool server: `kubectl get remotemcpserver/demo-tools -n kagent -o yaml`. Wait until `status.discoveredTools` is populated before running `init`.
- **`skills-init` container fails:** Inspect the pod logs: `kubectl logs -n kagent -l kagent=appa-guide -c skills-init`. If a directory conflict occurs, check for duplicate skill names or let the controller restart the pod.

## Uninstall

Choose the cleanup option that matches what you installed:

#### Remove only the demo release

Removes the demo releases and mock tools, leaving the cluster configuration intact:

```sh
helm uninstall appa-kagent-demo -n kagent --ignore-not-found
helm uninstall appa-runtime -n kagent --ignore-not-found
```

#### Restore stock images and remove OpenAPPA

Restores the kagent controller to stock images and uninstalls `appa-runtime` (persistent PVCs are retained):

```sh
kubectl delete agent appa-guide -n kagent --ignore-not-found --wait
helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version 0.9.12 -n kagent --reuse-values \
  --set registry=ghcr.io \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=kagent-dev/kagent/app \
  --set-string controller.agentImage.tag=0.9.12 \
  --force-conflicts --wait --timeout 10m
kubectl get deployment -A -l kagent \
  -o 'custom-columns=NAMESPACE:.metadata.namespace,NAME:.metadata.name,IMAGES:.spec.template.spec.containers[*].image'
# After verifying every affected Agent uses ghcr.io/kagent-dev/kagent/app:0.9.12:
helm uninstall appa-runtime -n appa --ignore-not-found
```

#### Remove kagent and its CRDs

Completely uninstalls the kagent controller and removes cluster-wide CustomResourceDefinitions:

```sh
helm uninstall kagent -n kagent --ignore-not-found
helm uninstall kagent-crds -n kagent --ignore-not-found
```

## Where next

- [How it works](/how-it-works) - Core concepts and label algebra.
- [Policy configuration](/contracts) - Syntax for tools, annotators, and authorities.
- [What is a battery](/batteries) - Maintained policy bundles.
- [Validation](/validation) - Offline policy validation and replay.
- [Kagent implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) - Adapter lifecycle and wire protocol.
