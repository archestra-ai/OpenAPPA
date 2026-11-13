#!/usr/bin/env bash
# envsubst takes the names to substitute as a literal '${VAR}' list.
# shellcheck disable=SC2016
# Deploy one stock Claude Code actor to try OpenAPPA at an Agent Substrate
# egress gateway. Substrate must be installed with `ate-setup deploy --openappa`.
#
#   integrations/substrate/deploy.sh          build, push, and create the actor
#   integrations/substrate/deploy.sh delete   remove the actor and its namespace
#
# Required environment:
#   WORKER_IMAGE        the installed Substrate's ateworker-gvisor image
#   CLAUDE_TOKEN_FILE   a file holding a `claude setup-token` OAuth token
#   GITHUB_TOKEN_FILE   a file holding a GitHub token, injected by the egress
#                       gateway into the actor's MCP requests
#
# Optional environment:
#   KUBE_CONTEXT        kubeconfig context (default: kind-substrate)
#   IMAGE_REPO          registry the cluster pulls from (default: localhost:5001)
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
context="${KUBE_CONTEXT:-kind-substrate}"
repo="${IMAGE_REPO:-localhost:5001}"
namespace=appa-substrate-demo
atespace=appa-substrate-demo
actor=agent
secret=appa-substrate-demo-credentials

k() { kubectl --context "$context" "$@"; }
ate() { kubectl ate --context "$context" "$@"; }

if [[ "${1:-}" == "delete" ]]; then
  ate delete actor "$actor" -a "$atespace" 2>/dev/null || true
  ate delete actor-template "$actor" -a "$atespace" 2>/dev/null || true
  k delete namespace "$namespace" --ignore-not-found
  echo "Deleted. The atespace and its namespace grant remain."
  exit 0
fi

: "${WORKER_IMAGE:?set WORKER_IMAGE to the installed ateworker-gvisor image}"
: "${CLAUDE_TOKEN_FILE:?set CLAUDE_TOKEN_FILE to a file holding a claude setup-token token}"
: "${GITHUB_TOKEN_FILE:?set GITHUB_TOKEN_FILE to a file holding a GitHub token}"
export WORKER_IMAGE

# Each run tags its own build, and the manifests pin the pushed digest.
tag="dev-$(date +%Y%m%d%H%M%S)"
build() {
  docker build -t "$repo/$1:$tag" "$2" >&2
  docker push "$repo/$1:$tag" >/dev/null
  docker inspect --format '{{index .RepoDigests 0}}' "$repo/$1:$tag"
}

echo "==> Building the agent image"
ACTOR_IMAGE="$(build appa-substrate-agent "$here/workload")"
export ACTOR_IMAGE

echo "==> Applying the namespace and worker pool"
envsubst '${WORKER_IMAGE}' <"$here/manifests/workerpool.yaml.tmpl" | k apply -f -

echo "==> Storing the credentials the egress gateway injects"
k -n "$namespace" create secret generic "$secret" \
  --from-file=claude-code-oauth-token=<(tr -d '[:space:]' <"$CLAUDE_TOKEN_FILE") \
  --from-file=github-token=<(tr -d '[:space:]' <"$GITHUB_TOKEN_FILE") \
  --dry-run=client -o yaml | k apply -f -

echo "==> Granting atespace $atespace the Secrets of namespace $namespace"
policy="$(k -n ate-system get configmap k8s-credential-provider-namespace-policy \
  -o jsonpath='{.data.namespace-policy\.yaml}')"
updated="$(ATESPACE="$atespace" NAMESPACE="$namespace" python3 -c '
import os, sys, yaml
doc = yaml.safe_load(sys.stdin.read()) or {}
policies = doc.get("policies") or []
entry = next((p for p in policies if p.get("atespace") == os.environ["ATESPACE"]), None)
if entry is None:
    entry = {"atespace": os.environ["ATESPACE"], "allowedNamespaces": []}
    policies.append(entry)
if os.environ["NAMESPACE"] in entry["allowedNamespaces"]:
    sys.exit(3)
entry["allowedNamespaces"].append(os.environ["NAMESPACE"])
doc["policies"] = policies
print(yaml.safe_dump(doc, sort_keys=False))
' <<<"$policy")" && {
  k -n ate-system create configmap k8s-credential-provider-namespace-policy \
    --from-literal=namespace-policy.yaml="$updated" --dry-run=client -o yaml | k apply -f -
  # The provider reads its policy once, at startup.
  k -n ate-system rollout restart deployment/k8s-credential-provider
  k -n ate-system rollout status deployment/k8s-credential-provider --timeout=2m
} || echo "    already granted"

echo "==> Creating the atespace, the template, and the actor"
ate create atespace "$atespace" 2>/dev/null || echo "    atespace exists"
ate delete actor "$actor" -a "$atespace" 2>/dev/null || true
ate delete actor-template "$actor" -a "$atespace" 2>/dev/null || true
envsubst '${ACTOR_IMAGE}' <"$here/manifests/actor-template.yaml.tmpl" | ate create actor-template -f -
ate create actor "$actor" -a "$atespace" --template "$actor"
ate create egress-policy "$actor" -a "$atespace" -f "$here/manifests/egress-policy.yaml.tmpl"

cat <<EOF

Done. Open the actor's console through Substrate's router, and the egress
decisions beside it:

  kubectl ate --context $context proxy actor $actor 8000:8080 -a $atespace
  kubectl --context $context -n ate-system port-forward deploy/openappa 8001:8081
  open 'http://localhost:8000/?log-port=8001'
EOF
