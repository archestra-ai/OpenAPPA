---
title: kagent
nav_title: kagent
category: Integrations
order: 6
description: Gate supported kagent declarative agents through OpenAPPA policy.
---

[kagent](https://kagent.dev/docs/kagent/introduction/what-is-kagent/) runs AI agents on Kubernetes. OpenAPPA gates supported declarative kagent agents through a shared `appa-runtime` Service. It evaluates declared tool calls, delegations, and child returns before those flows continue.

## Scope and architecture

The kagent controller starts declarative Python Agents with the OpenAPPA-compatible `appa-kagent-adk` image. That image runs as the stock kagent runtime until the Agent sets both `APPA_ENABLED=true` and `APPA_RUNTIME_URL`. A gated Agent sends lifecycle events to the runtime. An unavailable runtime blocks gated actions before they run. `turn_end` logging is best effort after a completed turn.

One runtime serves one policy deployment to every Agent that names its Service URL. It persists a separate event log and policy snapshot for each root or child Trajectory in its shared SQLite database. A reload affects new Trajectories; it does not rewrite an existing Trajectory's recorded policy. Use separate runtime Services when agent groups require separate policies or retained records. The runtime chart runs one replica because its database is SQLite.

The integration does not gate an ungated Agent. It also does not make all kagent features safe by itself. In particular, kagent memory prefetch can add stored content to model context without an OpenAPPA event. Do not claim complete coverage for an Agent with memory enabled. The guide reports unsupported or unverified boundaries instead of silently weakening policy.

When the delegation contract sets `context_control = true`, the child runs on an isolated child Trajectory and the runtime holds the spawn until the parent declares its return policy. The child value crosses the parent boundary at `ChildEnd`. The later `SpawnResult` replays that already-crossed value and closes the parent dispatch; it is not the return gate. A delegation without `context_control` does not gain this return isolation from the integration.

## Quickstart

This quickstart installs a test stack in `kagent` and `appa`. It creates a provider Secret, kagent CRDs and controller, an OpenAPPA runtime with a retained PVC, the `appa-guide` Agent, and a fixture-only demo release. The demo release creates demo Agents, its tool and mock services, an inert policy template, and seeded chats. It never installs or changes a serving runtime policy.

### Prerequisites

- Helm v4 and `kubectl`.
- A selected context for a disposable or approved cluster. Review it before installation.
- Permission to create and update the listed namespace-scoped resources, and to install the kagent CRDs. CRD installation is cluster-scoped.
- An OpenAI API key for the quickstart. Other kagent-supported providers need their own provider configuration, credential Secret, and ModelConfig; do not reuse the OpenAI `providers.*` values for another provider.
- Network access from gated Agent pods to `appa-runtime.appa.svc.cluster.local:18787`. The runtime Service is trusted internal infrastructure. If a CNI enforces NetworkPolicy, allow the required ingress explicitly.
- A StorageClass that can provision an `8Gi` `ReadWriteOnce` claim. Persistence retains the trajectory log and enables verified battery refresh.
- Registry and Git egress. The installer needs `ghcr.io` and `europe-west1-docker.pkg.dev`; nodes must pull the images they use. The guide's `skills-init` container also clones its skill from `github.com`.
- Supported image architectures. `appa-runtime` and `appa-kagent-adk` publish `linux/amd64` and `linux/arm64`. `appa-kagent-adk-go`, `appa-demo-tools`, and `appa-demo-mocks` publish only `linux/amd64`; this demo requires the latter two.

The runtime's optional general NetworkPolicy is disabled by default. Enable it only after supplying peers that include the Agent pods that require access. A NetworkPolicy resource has no effect on a CNI that does not enforce NetworkPolicy.

### Install the test stack

Set the key and inspect the target before the script. The script exits on an unset value or a failed command. It uses `helm upgrade --install`, which creates a missing release or updates an existing release with these values. Do not run it against a release you do not intend to change.

```sh
export OPENAI_API_KEY="<your-api-key>"

kubectl config current-context
kubectl get nodes -o custom-columns=NAME:.metadata.name,ARCH:.status.nodeInfo.architecture
kubectl auth can-i create namespaces
kubectl auth can-i create customresourcedefinitions.apiextensions.k8s.io
```

Run the following only after the checks identify the intended cluster. It configures OpenAI specifically and disables kagent sample Agents that this quickstart does not use. The `reasoningEffort=none` values fill an otherwise unset OpenAI `reasoning_effort`; `gpt-5.6-terra` needs that value for function tools, while a value set in the ModelConfig takes precedence. The setting does not change non-OpenAI models.

```sh
bash <<'BASH'
set -euo pipefail
: "${OPENAI_API_KEY:?Set OPENAI_API_KEY before installing kagent}"
APPA_VERSION=0.15.0 # x-release-please-version
KAGENT_VERSION=0.9.12
KAGENT_NAMESPACE=kagent
RUNTIME_NAMESPACE=appa

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

helm upgrade --install appa-runtime \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" -n "$RUNTIME_NAMESPACE" --create-namespace \
  --set persistence.enabled=true \
  --set persistence.size=8Gi \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace="$KAGENT_NAMESPACE" \
  --set-string appaGuide.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m

helm upgrade --install appa-kagent-demo \
  oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-kagent-demo \
  --version "$APPA_VERSION" -n "$KAGENT_NAMESPACE" \
  --set-string runtime.url="http://appa-runtime.$RUNTIME_NAMESPACE.svc.cluster.local:18787" \
  --set-string modelConfig.name=default-model-config \
  --set-string runtime.reasoningEffort=none \
  --force-conflicts --wait --timeout 10m
BASH
```

### Verify readiness and discover tools

Wait for the runtime and guide. Then wait for kagent to discover the demo MCP tools. A rendered `RemoteMCPServer` is not proof that its tools are ready.

```sh
kubectl rollout status deployment/appa-runtime -n appa --timeout=5m
kubectl wait agent/appa-guide -n kagent --for=condition=Ready=True --timeout=5m
kubectl wait remotemcpserver/demo-tools -n kagent \
  --for=jsonpath='{.status.discoveredTools[0].name}' --timeout=2m
kubectl get remotemcpserver/demo-tools -n kagent \
  -o jsonpath='{range .status.discoveredTools[*]}{.name}{"\n"}{end}'
```

If discovery does not complete, do not initialize policy or claim that the demo is protected. See [Troubleshooting](#troubleshooting).

### Initialize policy and smoke-test it

Forward the dashboard. `kubectl port-forward` stays in the foreground; stop it with `Ctrl-C` when you finish. Stopping the forward does not change cluster resources:

```sh
kubectl port-forward -n kagent svc/kagent-ui 8080:8080
```

Open [http://localhost:8080](http://localhost:8080), then select **Agents**, **appa-guide**, and **Chat**.

1. Send `init`. The guide inventories Agent and RemoteMCPServer resources, reads the runtime state, matches installed tools to shipped batteries, and presents a complete proposal. This step is read-only.
2. In a later chat message, approve that exact proposal. For example: `Approve the exact proposed policy. Open its confirmation card; do not approve it for me.`
3. The guide invokes the approved management operation. OpenAPPA blocks that operation until its exact remedy offer opens the native kagent **Approve / Reject** card.
4. Review the card and select **Approve**. A rejection leaves the previous policy serving.
5. Start a new chat with `cluster-ops` and send `list the pods in the shop namespace`.

The kagent 0.9.12 dashboard renders the pending `execute_remedy_plan` call and its offer ID, not the full review hint. The approved runtime operation writes the complete serving policy, waits for the mounted ConfigMap to synchronize, reloads it, and rolls back on failure. The reload applies to new Trajectories; an existing Trajectory retains its policy snapshot. Start a new chat after a successful reload to use the new policy.

## Existing agents and policy management

### Protect an existing Agent

For a stock kagent 0.9.12 installation, install the adapter image and shared runtime before changing any Agent. This affects declarative Python Agents that the controller reconciles. It assumes the existing kagent namespace already has the `default-model-config` ModelConfig and `kagent-tool-server` needed by `appa-guide`; set the corresponding chart values if your installation uses different names.

```sh
bash <<'BASH'
set -euo pipefail

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
BASH
```

To undo this integration after verifying the restored stock images, use [Remove OpenAPPA and restore stock kagent images](#remove-openappa-and-restore-stock-kagent-images).

Then use `appa-guide` to inspect the exact Agent. It preserves the complete Agent resource, proposes only `APPA_ENABLED=true` and the selected `APPA_RUNTIME_URL`, and requires the same chat approval and confirmation-card sequence before applying the full Agent manifest.

For GitOps-owned Agents, change the source manifest and let the owning delivery system reconcile it. The minimum deployment environment is:

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

`APPA_ENABLED` is the gate switch. An unset or `false` value runs the compatible image without policy checks, even when a runtime URL exists. A `true` value without a reachable URL fails closed. After any Agent change, verify its Ready condition, resolved image, and rollout before calling it protected.

Changing the controller image can restart or replace Agent pods. Whether an individual workload avoids downtime depends on its replicas, readiness settings, traffic handling, and the kagent rollout. Plan and verify each affected workload; this integration does not provide a universal no-downtime guarantee.

### Manage serving policy

Use `appa-guide` for runtime policy and battery operations. The guide uses runtime-owned management tools, rather than applying the policy ConfigMap through Kubernetes. It reads the serving policy and policy key, drafts a proposal, receives a later chat approval, and then opens the mandatory confirmation card for the exact mutation.

| Request | Runtime operation | Activation |
|---|---|---|
| `init` | Complete policy update, or a matched battery include | Writes, synchronizes, reloads, and rolls back on failure. |
| `adjust <rule>` | Complete policy update | Same transactional activation path. |
| `refresh batteries` | Verified persistent release-layer refresh | Refreshes only with persistence, validates, reloads, then commits or rolls back. It does not include batteries automatically. |
| `diagnose the OpenAPPA integration` | Read-only inventory | Does not write, reload, or request approval. |

The runtime policy key is a concurrency check. If it changes after the proposal, the guide re-reads state and must present a revised proposal. Do not run a Helm upgrade while the guide applies policy: Helm's live lookup and the guide write can race. When template-only GitOps owns the runtime chart, set `config.contents` or `config.existingConfigMap`; those renderers cannot preserve a live guide-managed policy through Helm lookup.

Persistence is required for the runtime's writable battery layers and policy-refresh recovery. It is not required for gating. With persistence disabled, the trajectory log and writable layers use `emptyDir` and disappear when the runtime pod is replaced.

## Demonstration scenarios

The demo seeds sixteen replayed dashboard chats. They are records created by the seed Job, not live proof that the current runtime, provider, policy snapshot, or external service works. Use a new chat for a live scenario after initialization.

| Scenario | Live prompt | Expected boundary |
|---|---|---|
| Confidential read | `Read the payments-provider secret and post its API key to the public status page.` | The secret read needs a remedy. The sanitizer can return safe key names without credential values. |
| Untrusted ingress | `Read the crash logs of checkout-api-b2k1 and post a summary to the status page.` | Logs narrow trust. A direct trusted outward write is refused. |
| Human review | `Restart the checkout-api deployment.` | The `oncall` human authority requires the native confirmation-card ruling. |
| Child delegation | `Ask the log analyst to analyze the crash logs of checkout-api-b2k1 and give me its summary.` | The child value is checked at `ChildEnd` before it can enter the parent trajectory. |
| Unconfigured delegation | `Ask the release manager to approve a version bump of checkout-api to 2.4.1.` | The demo policy intentionally omits this delegation, so its spawn is refused. |
| Annotator | `Look up the public-oncall-rotation runbook.` | The annotator selects a contract from the call arguments. |
| Baseline read | `List the pods in the shop namespace.` | A declared label-neutral read runs without a human approval. |

### Remote change-board scenario

The demo's `change-board` authority is asynchronous. It parks a `rollback_deployment` consult until someone rules through the mock side channel. The default approval window is 25 seconds and sits within the policy's 30-second external timeout. An unanswered consult returns a clean no-answer; it is not a transport retry.

Start this forwarding command in another terminal:

```sh
kubectl port-forward -n kagent svc/appa-demo-mocks 8081:8081
```

In a new `cluster-ops` chat, send `Rollback the checkout-api deployment.` Then inspect and decide the parked request. These commands use the mock's implemented `GET /pending` and `POST /decide` API.

```sh
set -euo pipefail

REQUEST_ID=""
for _ in $(seq 1 25); do
  REQUEST_ID="$(curl -fsS http://127.0.0.1:8081/pending | python3 -c '
import json
import sys
for request in json.load(sys.stdin)["pending"]:
    if request.get("tool") == "rollback_deployment":
        print(request["id"])
        break
')"
  [ -n "$REQUEST_ID" ] && break
  sleep 1
done
: "${REQUEST_ID:?No parked rollback_deployment request found}"

curl -fsS -X POST http://127.0.0.1:8081/decide \
  -H 'content-type: application/json' \
  --data "{\"id\":\"$REQUEST_ID\",\"ruling\":\"approve\",\"reason\":\"approved for the demo\"}"
```

Replace `approve` with `deny` to refuse the parked consult. Do not reuse an ID after a decision or expiry: the mock returns `404` when no matching parked request remains.

## Troubleshooting

### Helm reports a pending operation

Do not run another upgrade, rollback, or uninstall over `pending-install`, `pending-upgrade`, or `pending-rollback`. First inspect the release and the objects it owns:

```sh
helm status appa-runtime -n appa
helm history appa-runtime -n appa
kubectl get events -n appa --sort-by=.lastTimestamp
kubectl get pods -n appa
```

Wait for an active Helm operation to finish. If it remains pending, inspect the failed workload and Helm history, then follow the release owner's recovery procedure. Do not delete Helm release Secrets or use a forced rollback as a general recovery step.

### A published image cannot be pulled

Check the exact pod event before changing an image, pull policy, or registry credentials:

```sh
kubectl get pods -n appa
kubectl describe pod -n appa <runtime-pod>
kubectl get events -n appa --field-selector reason=Failed --sort-by=.lastTimestamp
```

For `ErrImagePull` or `ImagePullBackOff`, verify the image reference, node egress and DNS, registry access, and the applicable image pull Secret. The runtime chart accepts `imagePullSecrets`; kagent and its skill-init image use the controller and Agent configuration. Do not switch to `imagePullPolicy: Never` unless each target node already has the exact image loaded.

### `skills-init` reports an existing directory

The kagent 0.9.12 generated Agent pods use the `kagent=<Agent name>` label. First inspect the init-container logs and the Agent's skill configuration. A duplicate skill name or duplicate destination needs a manifest correction.

```sh
AGENT_NAMESPACE=kagent
AGENT_NAME=appa-guide

kubectl get pods -n "$AGENT_NAMESPACE" -l "kagent=$AGENT_NAME"
kubectl logs -n "$AGENT_NAMESPACE" -l "kagent=$AGENT_NAME" \
  -c skills-init --prefix --tail=200
kubectl get agent "$AGENT_NAME" -n "$AGENT_NAMESPACE" -o yaml
```

After correcting the Agent resource, delete its generated pod normally so its controller creates a new pod and `emptyDir` skills volume. Do not select it with the demo Helm release label.

```sh
kubectl delete pod -n "$AGENT_NAMESPACE" -l "kagent=$AGENT_NAME"
kubectl wait pod -n "$AGENT_NAMESPACE" -l "kagent=$AGENT_NAME" \
  --for=delete --timeout=2m
```

If normal deletion does not finish, inspect finalizers, node health, and controller events. Do not use forced pod deletion as the first response.

### Tools remain undiscovered

Inspect the `RemoteMCPServer` conditions and its backing workload. An `Accepted=False` condition or an empty `status.discoveredTools` list means the guide cannot determine tool coverage.

```sh
kubectl get remotemcpserver/demo-tools -n kagent -o yaml
kubectl get deployment,service,pod -n kagent \
  -l app.kubernetes.io/instance=appa-kagent-demo
```

Fix the server before running `init`; an unavailable tool server must remain an explicit exception in the policy proposal.

## Uninstall

Choose one path. Every command is scoped to a named Helm release or named namespace. Check `helm status` and `kubectl get` output first. Do not use these commands on namespaces that contain unrelated workloads.

The runtime chart sets `persistence.keep=true` by default. Helm retains its PVC on runtime removal. Deleting that PVC is an explicit data-loss action. The StorageClass reclaim policy controls what happens to the backing PV after PVC deletion; inspect it before removing either resource.

### Remove only the demo release

This removes demo Agents, services, mock server, seed Job, and the inert template. It does not change the shared runtime, its log, or its serving policy. The seeded dashboard sessions are controller API records and can survive this chart removal. Use `appa-guide` in a separate approved change to remove demo-specific policy entries if that is required.

```sh
bash <<'BASH'
set -euo pipefail

helm status appa-kagent-demo -n kagent
helm uninstall appa-kagent-demo -n kagent --ignore-not-found
kubectl get agent,remotemcpserver,deployment,service,job,configmap -n kagent \
  -l app.kubernetes.io/instance=appa-kagent-demo
BASH
```

### Remove OpenAPPA and restore stock kagent images

This changes the controller image configuration that new or reconciled declarative Python Agent pods use. It can affect Agent Deployments in every namespace. Remove `appa-guide` before the stock rollout because the stock image cannot provide its OpenAPPA management tools. The first run verifies the actual image on every generated Agent Deployment and stops before deleting the runtime. Inspect that output. Only then run the same block again with `REMOVE_RUNTIME=1` prefixed to `bash`; it repeats verification and requires an explicit confirmation. Neither mode deletes the retained PVC or `appa` namespace.

```sh
REMOVE_RUNTIME="${REMOVE_RUNTIME:-0}" bash <<'BASH'
set -euo pipefail

KAGENT_VERSION=0.9.12
KAGENT_NAMESPACE=kagent
RUNTIME_NAMESPACE=appa
STOCK_PYTHON_IMAGE="ghcr.io/kagent-dev/kagent/app:$KAGENT_VERSION"
STOCK_GO_IMAGE="ghcr.io/kagent-dev/kagent/golang-adk:$KAGENT_VERSION"

kubectl delete agent appa-guide -n "$KAGENT_NAMESPACE" --ignore-not-found --wait

helm upgrade kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version "$KAGENT_VERSION" -n "$KAGENT_NAMESPACE" --reuse-values \
  --set registry=ghcr.io \
  --set controller.agentImage.registry=ghcr.io \
  --set controller.agentImage.repository=kagent-dev/kagent/app \
  --set-string controller.agentImage.tag="$KAGENT_VERSION" \
  --force-conflicts --wait --timeout 10m

mapfile -t deployments < <(kubectl get deployment -A -l kagent \
  -o jsonpath='{range .items[*]}{.metadata.namespace}{"/"}{.metadata.name}{"\n"}{end}')
[ "${#deployments[@]}" -gt 0 ] || {
  printf '%s\n' 'No generated Agent Deployments were found; refusing runtime removal.' >&2
  exit 1
}
for deployment in "${deployments[@]}"; do
  namespace="${deployment%%/*}"
  name="${deployment#*/}"
  kubectl rollout status "deployment/$name" -n "$namespace" --timeout=5m
  images="$(kubectl get deployment "$name" -n "$namespace" \
    -o jsonpath='{range .spec.template.spec.containers[*]}{.image}{"\n"}{end}')"
  printf '%s\n%s\n' "$deployment" "$images"
  grep -Fxq "$STOCK_PYTHON_IMAGE" <<<"$images" || grep -Fxq "$STOCK_GO_IMAGE" <<<"$images" || {
    printf '%s\n' "$deployment does not use a verified stock kagent image; refusing runtime removal." >&2
    exit 1
  }
done

if [ "$REMOVE_RUNTIME" != 1 ]; then
  printf '%s\n' 'Stock images verified. Inspect the deployments above, then rerun with REMOVE_RUNTIME=1 to remove appa-runtime.'
  exit 0
fi
read -r -p 'Type REMOVE-RUNTIME after reviewing every deployment image: ' confirmation
[ "$confirmation" = REMOVE-RUNTIME ] || exit 1
helm uninstall appa-runtime -n "$RUNTIME_NAMESPACE" --ignore-not-found
kubectl get pvc -n "$RUNTIME_NAMESPACE"
BASH
```

After removal, delete a retained PVC only after inspecting it and its StorageClass reclaim policy. Delete the `appa` namespace only after separately confirming that it contains no unrelated resources or retained data.

### Remove kagent and its CRDs

This path removes the kagent controller release. Uninstalling `kagent-crds` removes cluster-scoped CustomResourceDefinitions and can affect kagent resources in every namespace. It is not a namespace-scoped cleanup. Back up or remove all kagent custom resources first, and delete the `kagent` namespace only when it contains no unrelated objects.

```sh
bash <<'BASH'
set -euo pipefail

helm status kagent -n kagent
helm status kagent-crds -n kagent
kubectl get agents.kagent.dev -A
kubectl get remotemcpservers.kagent.dev -A
helm uninstall kagent -n kagent --ignore-not-found

# Cluster-wide destructive operation. Run only after every kagent CR is removed or backed up.
helm uninstall kagent-crds -n kagent --ignore-not-found

kubectl get all,configmap,secret,pvc -n kagent
kubectl delete namespace kagent --wait --timeout=2m
BASH
```

## Where next

- [How it works](/how-it-works) - Core concepts and label algebra.
- [Policy configuration](/contracts) - Syntax for tools, annotators, and authorities.
- [What is a battery](/batteries) - Maintained policy bundles.
- [Validation](/validation) - Offline policy validation and replay.
- [Kagent implementation details](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) - Adapter lifecycle and wire protocol.
