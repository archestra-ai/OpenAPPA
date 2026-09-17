#!/usr/bin/env bash
# Runs the appa-guide browser row against an installed demo stack.
set -euo pipefail
cd "$(dirname "$0")"

namespace=${APPA_NAMESPACE:-kagent}
model_config=${APPA_MODEL_CONFIG:-default-model-config}
runtime_url=${APPA_RUNTIME_URL:-http://appa-runtime.appa.svc.cluster.local:18787}
runtime_namespace=${APPA_RUNTIME_NAMESPACE:-appa}
target=appa-guide-e2e-fixture
created=false

cleanup() {
  if [ "$created" = true ]; then
    kubectl delete agent "$target" -n "$namespace" --ignore-not-found >/dev/null
  fi
}
trap cleanup EXIT

kubectl create -f - <<EOF
apiVersion: kagent.dev/v1alpha2
kind: Agent
metadata:
  name: $target
  namespace: $namespace
spec:
  type: Declarative
  description: Fixture Agent for appa-guide migration.
  declarative:
    systemMessage: List Kubernetes pods when asked.
    modelConfig: $model_config
    tools:
      - type: McpServer
        mcpServer:
          name: demo-tools
          kind: RemoteMCPServer
          toolNames: [list_pods]
    deployment:
      env:
        - name: EXISTING_SETTING
          value: preserve-me
EOF
created=true
kubectl wait agent/"$target" -n "$namespace" --for=condition=Ready=True --timeout=5m

(
  cd ui
  APPA_UI_E2E=1 APPA_AGENT=appa-guide APPA_GUIDE_TARGET="$target" \
    uv run --with playwright --with "pytest>=8" \
    pytest -v test_guide_ui.py
)

kubectl get agent "$target" -n "$namespace" -o json | jq -e \
  --arg model "$model_config" --arg runtime "$runtime_url" '
  .spec.description == "Fixture Agent for appa-guide migration." and
  .spec.type == "Declarative" and
  .spec.declarative.systemMessage == "List Kubernetes pods when asked." and
  .spec.declarative.modelConfig == $model and
  (.spec.declarative.tools == [{
    "type": "McpServer",
    "mcpServer": {
      "name": "demo-tools",
      "kind": "RemoteMCPServer",
      "toolNames": ["list_pods"]
    }
  }]) and
  (.spec.declarative.deployment.env as $env |
    any($env[]; .name == "EXISTING_SETTING" and .value == "preserve-me") and
    any($env[]; .name == "APPA_ENABLED" and .value == "true") and
    any($env[]; .name == "APPA_RUNTIME_URL" and .value == $runtime))
' >/dev/null

kubectl get configmap appa-runtime-policy -n "$runtime_namespace" -o json | jq -e '
  .data["appa.toml"] | contains("batteries/github/appa.toml")
' >/dev/null
