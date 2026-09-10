---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Protect kagent declarative Python Agents with OpenAPPA policy.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents natively on [Kubernetes](https://kubernetes.io/docs/home/). OpenAPPA adds flow control to these [Agents](https://kagent.dev/docs/kagent/concepts/agents/), checking tool calls, subagents, and data flows against deterministic policy before any action runs.

## How it works

:::fig-kagent:::

The OpenAPPA plugin intercepts tool calls, subagents, and return values before they execute:

- **Enforce policy:** Denied actions stop immediately. When human review is required, kagent requests approval in chat or via A2A.
- **Isolate subagents:** Subagent outputs are validated against policy before the parent agent can see them.
- **Shared runtime:** Agents connect to an `appa-runtime` service that evaluates policy and records audit logs. New policies apply automatically to new chats.

## Quickstart

Installs the kagent controller and agent runtime with the OpenAPPA plugin, a pre-configured `appa-runtime` service, and demo agents with tools for showcase scenarios.

*(Already running kagent? Skip to [Protect existing agents](#protect-existing-agents).)*

#### Prerequisites

- [kind](https://kind.sigs.k8s.io/docs/user/quick-start/) (or any local [Kubernetes cluster](https://kubernetes.io/docs/setup/)), [Helm](https://helm.sh/docs/intro/install/) v4, and [kubectl](https://kubernetes.io/docs/tasks/tools/).
- An [OpenAI API key](https://platform.openai.com/api-keys) (or another [supported provider](https://kagent.dev/docs/kagent/supported-providers/)).

#### 1. Deploy the demo stack

Export your OpenAI API key:

```sh
export OPENAI_API_KEY="your-api-key"
```

Deploy the demo:

```sh
APPA_VERSION=0.17.1 # x-release-please-version
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
  --set kmcp.podSecurityContext.runAsUser=65532 \
  --set kmcp.podSecurityContext.runAsGroup=65532 \
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
echo ""
```

To clean up the demo later, see [Uninstall](#uninstall).

#### 2. Open the dashboard

Forward the dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Open [http://localhost:8080](http://localhost:8080), select **Agents** &rarr; **`cluster-ops`** &rarr; **Chat**, and try the demonstration scenarios below.

## Demonstration scenarios

In the dashboard ([http://localhost:8080](http://localhost:8080)), open **Agents** &rarr; **`cluster-ops`** &rarr; **Chat**. Chat history contains five pre-recorded runs, one for each scenario below. The dynamic input rules chat includes both runbook prompts. Other demo agents have no pre-seeded chats. Inspect these runs, or start a new chat to test the prompts live:

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

Restarting a deployment requires approval. OpenAPPA pops up an interactive **Approve / Reject** card in chat before the restart proceeds.

#### 4. Subagents

```text
Ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary.
```

The subagent runs in an isolated session. Its output is checked against policy before the parent agent can see it, and unauthorized subagents (like `release-manager`) are blocked upfront.

#### 5. Dynamic input rules

Compare how OpenAPPA evaluates the same tool dynamically based on its arguments:

**Allowed public runbook:**

```text
Look up the public-oncall-rotation runbook.
```

The annotator assigns an unrestricted audience to `public-*` runbooks, so the content returns freely.

**Internal runbook with restricted remedy:**

```text
Look up the ops-database-failover runbook.
```

The annotator tags `ops-*` runbooks as internal. OpenAPPA blocks the unconstrained read and requires the agent to accept a restricted-reader remedy before retrieving the operational runbook.

## Protect existing agents

If you already run kagent with your own agents, use `appa-guide` to configure policy and protect them conversationally.

#### 1. Deploy the plugin image and runtime

Update the controller with the `appa-kagent-adk` plugin image and deploy `appa-runtime`:

```sh
APPA_VERSION=0.17.1 # x-release-please-version
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
  --set persistence.enabled=false \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace="$KAGENT_NAMESPACE" \
  --set-string appaGuide.modelConfig=default-model-config \
  --set-string appaGuide.toolServer.name=kagent-tool-server \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m

kubectl rollout status deployment/appa-runtime -n "$RUNTIME_NAMESPACE" --timeout=5m
kubectl wait agent/appa-guide -n "$KAGENT_NAMESPACE" \
  --for=condition=Ready=True --timeout=5m
echo ""
```

To retain trajectory audit logs and battery updates across restarts, enable persistence (requires a `ReadWriteOnce` [StorageClass](https://kubernetes.io/docs/concepts/storage/storage-classes/)):

```sh
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
echo ""
```

To remove the plugin and runtime later, see [Restore stock images and remove OpenAPPA](#restore-stock-images-and-remove-openappa).

#### 2. Initialize policy with appa-guide

Forward the dashboard:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Open [http://localhost:8080](http://localhost:8080), select **Agents** &rarr; **appa-guide** &rarr; **Chat**, and send:

```text
init
```

`appa-guide` scans your cluster tools, matches relevant [batteries](/batteries) (like GitHub or Slack), and drafts starting policy rules.

Click **Approve** on the confirmation card to activate the policy.

#### 3. Protect your agents with appa-guide

In chat with `appa-guide`, protect an existing agent:

```text
protect <your-agent-name>
```

Or protect every declarative agent at once:

```text
protect all agents
```

`appa-guide` inspects the agent, presents the configuration diff, and prompts for card approval to apply it.

#### Manual configuration (GitOps)

To configure agents via GitOps, add these environment variables to the Agent spec:

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

When `APPA_ENABLED` is true, all tool calls route through OpenAPPA (failing closed if unreachable). Unset or `false` runs the agent without protection.

#### Multiple policies across agent groups

Agents sharing an `APPA_RUNTIME_URL` share a policy. To give agent groups distinct policies, deploy separate `appa-runtime` releases, each with its own service URL.

## Manage policy with appa-guide

Use `appa-guide` in chat to inspect and modify policies conversationally:

- **`init`**: Scans tools and generates a starting policy.
- **`show policy`** (or **`explain policy`**): Explains active security rules, protected tools, and included batteries in plain English.
- **`adjust <rule>`**: Modifies specific tool contracts, trust levels, or audience boundaries.
- **`refresh batteries`**: Updates included battery definitions (requires persistence).
- **`diagnose the OpenAPPA integration`**: Runs read-only health checks on connectivity and configuration.

Changes take effect immediately once approved on the native confirmation card.

## Troubleshooting

- **Tools do not appear:** Inspect your tool server: `kubectl get remotemcpserver -n kagent -o yaml`. Ensure `status.discoveredTools` is populated before running `init`.
- **Agent pod fails to start:** Inspect the pod logs: `kubectl logs -n kagent -l kagent=appa-guide`.

## Uninstall

#### Remove only the demo release

Removes demo releases and mock tools:

```sh
helm uninstall appa-kagent-demo -n kagent --ignore-not-found
helm uninstall appa-runtime -n kagent --ignore-not-found
echo ""
```

#### Restore stock images and remove OpenAPPA

Restores the controller to stock images and uninstalls `appa-runtime`:

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
echo ""
```

#### Remove kagent and its CRDs

```sh
helm uninstall kagent -n kagent --ignore-not-found
helm uninstall kagent-crds -n kagent --ignore-not-found
echo ""
```

## Where next

- [How it works](/how-it-works) - Core concepts and label algebra.
- [Policy configuration](/contracts) - Syntax for tools, annotators, and authorities.
- [What is a battery](/batteries) - Maintained policy bundles.
- [Validation](/validation) - Offline policy validation and replay.
- [Kagent implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) - Plugin lifecycle and wire protocol.
