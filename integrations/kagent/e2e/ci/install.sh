#!/usr/bin/env bash
# Installs kagent, the shared runtime chart, and the fixture-only demo
# chart on the current cluster. The images come from kind-up.sh. The lane
# has no UI, guide, go cell, seed job, or unused tool server. It waits for
# every pod the subset drives.
#
#   OPENROUTER_API_KEY=… ./install.sh
#
# The key lands in an e2e-owned Secret. Only the Agent ModelConfig reads it;
# deterministic demo sanitizers run through the mock policy service.
#
# Env, with defaults:
#   APPA_E2E_NAMESPACE    kagent    the release namespace
#   APPA_E2E_RUNTIME_NAMESPACE appa the runtime release namespace
#   KAGENT_VERSION        0.9.12    the kagent chart version
#   APPA_E2E_IMAGE_TAG    ci        the tag the images carry
#   APPA_E2E_MODEL                  the Agent model
#   APPA_E2E_BASE_URL               the Agent model endpoint
#   APPA_E2E_WAIT_SECONDS 300       the budget for one rollout
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
chart=$(cd "$here/../../demo/chart" && pwd)
runtime_chart=$(cd "$here/../../../../charts/appa-runtime" && pwd)

namespace=${APPA_E2E_NAMESPACE:-kagent}
runtime_namespace=${APPA_E2E_RUNTIME_NAMESPACE:-appa}
kagent_version=${KAGENT_VERSION:-0.9.12}
tag=${APPA_E2E_IMAGE_TAG:-ci}
model=${APPA_E2E_MODEL:-openai/gpt-5.6-luna}
base_url=${APPA_E2E_BASE_URL:-https://openrouter.ai/api/v1}
wait_seconds=${APPA_E2E_WAIT_SECONDS:-300}
key=${OPENROUTER_API_KEY:-}
model_config=appa-e2e-model
runtime_url="http://appa-runtime.${runtime_namespace}.svc.cluster.local:18787"
policy=$(mktemp)
trap 'rm -f "$policy"' EXIT

if [ -z "$key" ]; then
  echo "install: OPENROUTER_API_KEY is empty. Both models refuse every call without it." >&2
  exit 1
fi

# kagent ships ten sample agents and three tool charts, all on by default.
# The A2A matrix uses none of them, so this install turns them off.
extras=(
  k8s-agent
  kgateway-agent
  istio-agent
  promql-agent
  observability-agent
  argo-rollouts-agent
  helm-agent
  cilium-policy-agent
  cilium-manager-agent
  cilium-debug-agent
  grafana-mcp
  querydoc
  kagent-tools
)
disable=()
for extra in "${extras[@]}"; do
  disable+=(--set "$extra.enabled=false")
done

echo "== helm install kagent-crds $kagent_version"
helm upgrade --install kagent-crds oci://ghcr.io/kagent-dev/kagent/helm/kagent-crds \
  --version "$kagent_version" -n "$namespace" --create-namespace --wait

# controller.agentImage puts every declarative python agent of the
# cluster on the runtime image. The pull policy Never keeps the node on
# the copy kind-up.sh loaded.
echo "== helm install kagent $kagent_version"
helm upgrade --install kagent oci://ghcr.io/kagent-dev/kagent/helm/kagent \
  --version "$kagent_version" -n "$namespace" \
  --set controller.agentImage.registry=docker.io \
  --set controller.agentImage.repository=library/appa-kagent-adk \
  --set-string controller.agentImage.tag="$tag" \
  --set controller.agentImage.pullPolicy=Never \
  --set ui.replicas=0 \
  "${disable[@]}" \
  --wait --timeout 10m

# The demo chart consumes an existing ModelConfig. This test fixture owns
# its own credential instead of making either OpenAPPA chart own kagent's
# provider configuration.
key_base64=$(printf %s "$key" | base64 | tr -d '\n')
kubectl apply -f - <<EOF
apiVersion: v1
kind: Secret
metadata:
  name: $model_config
  namespace: $namespace
type: Opaque
data:
  OPENAI_API_KEY: $key_base64
---
apiVersion: kagent.dev/v1alpha2
kind: ModelConfig
metadata:
  name: $model_config
  namespace: $namespace
spec:
  provider: OpenAI
  model: $model
  apiKeySecret: $model_config
  apiKeySecretKey: OPENAI_API_KEY
  openAI:
    baseUrl: $base_url
EOF

# An empty reasoningEffort leaves reasoning_effort out of every
# request. The fill answers gpt-5.6 on chat completions, and other
# models refuse the field.
# The matrix proves that an unanswered consult expires. Two seconds keeps
# that behavior while avoiding the chart's 25-second operator window in CI.
demo_values=(
  --set agents.go.enabled=false
  --set seed.enabled=false
  --set-string modelConfig.name="$model_config"
  --set-string runtime.url="$runtime_url"
  --set-string runtime.reasoningEffort=""
  --set mocks.approvalWindowSeconds=2
  --set tools.image.repository=docker.io/library/appa-demo-tools
  --set-string tools.image.tag="$tag"
  --set tools.image.pullPolicy=Never
  --set mocks.image.repository=docker.io/library/appa-demo-mocks
  --set-string mocks.image.tag="$tag"
  --set mocks.image.pullPolicy=Never
)

# Render the inert template before creating any gated Agent. The runtime
# chart copies it into the serving ConfigMap for this deterministic lane.
helm template appa-kagent-demo "$chart" -n "$namespace" \
  "${demo_values[@]}" --show-only templates/configmaps.yaml \
  | kubectl create --dry-run=client -f - -o jsonpath='{.data.appa\.toml}' \
  >"$policy"

echo "== helm install appa-runtime in $runtime_namespace"
helm upgrade --install appa-runtime "$runtime_chart" -n "$runtime_namespace" \
  --create-namespace \
  --set appaGuide.enabled=false \
  --set image.repository=docker.io/library/appa-runtime \
  --set-string image.tag="$tag" \
  --set image.pullPolicy=Never \
  --set-file config.contents="$policy" \
  --wait --timeout 10m

echo "== helm install fixture-only appa-kagent-demo, model $model at $base_url"
helm upgrade --install appa-kagent-demo "$chart" -n "$namespace" \
  "${demo_values[@]}" --wait --timeout 10m

# helm waits for its own Deployments alone. The kagent controller
# compiles each Agent object into a Deployment after the release lands.
# The subset waits for those by hand.
wait_for_deployment() {
  local target_namespace=$1 name=$2 deadline=$((SECONDS + wait_seconds))
  while ! kubectl -n "$target_namespace" get "deploy/$name" >/dev/null 2>&1; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "install: the controller never created deploy/$name" >&2
      return 1
    fi
    sleep 5
  done
  kubectl -n "$target_namespace" rollout status "deploy/$name" --timeout="${wait_seconds}s"
}

echo "== waiting for deploy/appa-runtime"
wait_for_deployment "$runtime_namespace" appa-runtime
for deployment in appa-demo-mocks demo-tools cluster-ops log-analyst release-manager; do
  echo "== waiting for deploy/$deployment"
  wait_for_deployment "$namespace" "$deployment"
done

kubectl -n "$namespace" wait agent/cluster-ops --for=condition=Ready --timeout="${wait_seconds}s"
kubectl get pods -n "$runtime_namespace" -o wide
kubectl get pods -n "$namespace" -o wide
