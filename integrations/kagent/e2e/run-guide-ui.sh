#!/usr/bin/env bash
# Runs the appa-guide browser row against an installed demo stack.
set -euo pipefail
cd "$(dirname "$0")"

namespace=${APPA_NAMESPACE:-kagent}
target=${APPA_GUIDE_TARGET:-guide-fixture}

cleanup() {
  kubectl delete agent "$target" -n "$namespace" --ignore-not-found >/dev/null
}
trap cleanup EXIT

kubectl apply -f - <<EOF
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
    modelConfig: appa-demo-model
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
kubectl wait agent/"$target" -n "$namespace" --for=condition=Ready=True --timeout=5m

(
  cd ui
  APPA_UI_E2E=1 APPA_AGENT=appa-guide APPA_GUIDE_TARGET="$target" \
    uv run --with playwright --with "pytest>=8" --with pytest-rerunfailures \
    pytest -v test_guide_ui.py
)

kubectl get agent "$target" -n "$namespace" -o json | jq -e '
  .spec.declarative.deployment.env as $env |
  any($env[]; .name == "EXISTING_SETTING" and .value == "preserve-me") and
  any($env[]; .name == "APPA_ENABLED" and .value == "true") and
  any($env[]; .name == "APPA_RUNTIME_URL" and (.value | endswith(":18787")))
' >/dev/null
