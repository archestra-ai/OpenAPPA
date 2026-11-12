#!/usr/bin/env bash
# envsubst takes the names to substitute as a literal '${VAR}' list.
# shellcheck disable=SC2016
# Deploy OpenAPPA at an installed Agent Substrate's egress gateway, and one
# stock Claude Code actor to try it with.
#
#   integrations/substrate/deploy.sh          build, push, and create everything
#   integrations/substrate/deploy.sh delete   remove the actor and both namespaces
#
# Substrate must be installed with the processor configured:
#   --experimental-additional-egress-extproc-service=appa-system/appa-extproc:9443
#   --experimental-additional-egress-extproc-allow-mode-override
#   --experimental-additional-egress-extproc-timeout=60s
#
# Required environment:
#   WORKER_IMAGE        the installed Substrate's ateworker-gvisor image
#   CLAUDE_TOKEN_FILE   a file holding a `claude setup-token` OAuth token
#   GITHUB_TOKEN_FILE   a file holding a GitHub token: the actor's, injected
#                       by the egress gateway, and the policy's, for its
#                       repository-visibility lookups
#
# Optional environment:
#   KUBE_CONTEXT        kubeconfig context (default: kind-substrate)
#   IMAGE_REPO          registry the cluster pulls from (default: localhost:5001)
#   RUNTIME_IMAGE       OpenAPPA runtime image (default: ghcr.io/archestra-ai/appa-runtime:v0.42.0)
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
  k delete namespace "$namespace" appa-system --ignore-not-found
  echo "Deleted. The atespace and its namespace grant remain."
  exit 0
fi

: "${WORKER_IMAGE:?set WORKER_IMAGE to the installed ateworker-gvisor image}"
: "${CLAUDE_TOKEN_FILE:?set CLAUDE_TOKEN_FILE to a file holding a claude setup-token token}"
: "${GITHUB_TOKEN_FILE:?set GITHUB_TOKEN_FILE to a file holding a GitHub token}"
export WORKER_IMAGE
export RUNTIME_IMAGE="${RUNTIME_IMAGE:-ghcr.io/archestra-ai/appa-runtime:v0.42.0}"

build() {
  docker build -t "$repo/$1:latest" "$2" >&2
  docker push "$repo/$1:latest" >/dev/null
  docker inspect --format '{{index .RepoDigests 0}}' "$repo/$1:latest"
}

echo "==> Building the ext_proc processor and the agent image"
EXTPROC_IMAGE="$(build appa-extproc "$here/appa-extproc")"
ACTOR_IMAGE="$(build appa-substrate-agent "$here/workload")"
export EXTPROC_IMAGE ACTOR_IMAGE

echo "==> Deploying the enforcement point to appa-system"
envsubst '${EXTPROC_IMAGE} ${RUNTIME_IMAGE}' <"$here/manifests/enforcement.yaml.tmpl" | k apply -f -
k -n appa-system create configmap appa-policy --from-file=appa.toml="$here/policy/appa.toml" \
  --dry-run=client -o yaml | k apply -f -
k -n appa-system create secret generic appa-github-token \
  --from-file=token=<(tr -d '[:space:]' <"$GITHUB_TOKEN_FILE") --dry-run=client -o yaml | k apply -f -
k -n appa-system rollout restart deployment/appa-extproc
k -n appa-system rollout status deployment/appa-extproc --timeout=3m

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
  kubectl --context $context -n appa-system port-forward deploy/appa-extproc 8001:8081
  open 'http://localhost:8000/?gateway=http://localhost:8001/'
EOF
