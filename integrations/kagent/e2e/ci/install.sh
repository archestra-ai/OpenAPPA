#!/usr/bin/env bash
# Installs kagent and the demo chart on the current cluster, in the
# shape the live A2A matrix needs: the images kind-up.sh loaded, no UI,
# guide, go cell, seed job, or unused tool server, and one OpenAI-compatible
# endpoint for both the agents and the policy's sanitizers. It waits for
# every pod the subset drives.
#
#   OPENROUTER_API_KEY=… ./install.sh
#
# The key lands in the one Secret the chart renders. The agents read it
# through the ModelConfig and the runtime reads it as APPA_LLM_API_KEY.
#
# Env, with defaults:
#   APPA_E2E_NAMESPACE    kagent    the release namespace
#   KAGENT_VERSION        0.9.12    the kagent chart version
#   APPA_E2E_IMAGE_TAG    ci        the tag the images carry
#   APPA_E2E_MODEL                  the model both sides call
#   APPA_E2E_BASE_URL               the endpoint both sides call
#   APPA_E2E_WAIT_SECONDS 300       the budget for one rollout
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
chart=$(cd "$here/../../demo/chart" && pwd)

namespace=${APPA_E2E_NAMESPACE:-kagent}
kagent_version=${KAGENT_VERSION:-0.9.12}
tag=${APPA_E2E_IMAGE_TAG:-ci}
model=${APPA_E2E_MODEL:-openai/gpt-5.6-luna}
base_url=${APPA_E2E_BASE_URL:-https://openrouter.ai/api/v1}
wait_seconds=${APPA_E2E_WAIT_SECONDS:-300}
key=${OPENROUTER_API_KEY:-}

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
  --set controller.agentImage.repository=library/appa-kagent-quickstart \
  --set-string controller.agentImage.tag="$tag" \
  --set controller.agentImage.pullPolicy=Never \
  --set ui.replicas=0 \
  "${disable[@]}" \
  --wait --timeout 10m

# An empty reasoningEffort leaves reasoning_effort out of every
# request. The fill answers gpt-5.6 on chat completions, and other
# models refuse the field.
# The matrix proves that an unanswered consult expires. Two seconds keeps
# that behavior while avoiding the chart's 25-second operator window in CI.
echo "== helm install appa-kagent-demo, model $model at $base_url"
helm upgrade --install appa-kagent-demo "$chart" -n "$namespace" \
  --set agents.go.enabled=false \
  --set guide.enabled=false \
  --set seed.enabled=false \
  --set-string runtime.reasoningEffort="" \
  --set-string openai.apiKey="$key" \
  --set-string openai.model="$model" \
  --set-string openai.baseUrl="$base_url" \
  --set-string llm.model="$model" \
  --set-string llm.url="$base_url" \
  --set mocks.approvalWindowSeconds=2 \
  --set runtime.image.repository=docker.io/library/appa-kagent-quickstart \
  --set-string runtime.image.tag="$tag" \
  --set runtime.image.pullPolicy=Never \
  --set tools.image.repository=docker.io/library/appa-demo-tools \
  --set-string tools.image.tag="$tag" \
  --set tools.image.pullPolicy=Never \
  --set mocks.image.repository=docker.io/library/appa-demo-mocks \
  --set-string mocks.image.tag="$tag" \
  --set mocks.image.pullPolicy=Never \
  --wait --timeout 10m

# helm waits for its own Deployments alone. The kagent controller
# compiles each Agent object into a Deployment after the release lands.
# The subset waits for those by hand.
wait_for_deployment() {
  local name=$1 deadline=$((SECONDS + wait_seconds))
  while ! kubectl -n "$namespace" get "deploy/$name" >/dev/null 2>&1; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "install: the controller never created deploy/$name" >&2
      return 1
    fi
    sleep 5
  done
  kubectl -n "$namespace" rollout status "deploy/$name" --timeout="${wait_seconds}s"
}

for deployment in appa-runtime demo-tools cluster-ops log-analyst release-manager; do
  echo "== waiting for deploy/$deployment"
  wait_for_deployment "$deployment"
done

kubectl -n "$namespace" wait agent/cluster-ops --for=condition=Ready --timeout="${wait_seconds}s"
kubectl -n "$namespace" get pods -o wide
