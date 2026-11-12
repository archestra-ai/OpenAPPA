#!/usr/bin/env bash
# envsubst takes the names to substitute as a literal '${VAR}' list.
# shellcheck disable=SC2016
# Deploy the protected Claude Code actor onto an installed Agent Substrate.
#
#   integrations/substrate/deploy.sh          build, push, and create everything
#   integrations/substrate/deploy.sh delete   remove the actor, template, and namespace
#
# Required environment:
#   WORKER_IMAGE        the installed Substrate's ateworker-gvisor image, e.g. the
#                       ref `ate-setup publish worker-images --kind` prints
#   ARCHESTRA_MCP_URL   an Archestra MCP gateway URL, https://<host>/v1/mcp/<gateway>
#   CLAUDE_TOKEN_FILE   a file holding a `claude setup-token` OAuth token
#   ARCHESTRA_TOKEN_FILE  a file holding an Archestra token the gateway accepts
#
# Optional environment:
#   KUBE_CONTEXT        kubeconfig context (default: kind-substrate)
#   IMAGE_REPO          registry the workers pull from (default: localhost:5001)
#   APPA_VERSION        OpenAPPA release to bake in (default: latest)
#
# The tokens become one Kubernetes Secret in the demo namespace. The actor
# never sees them: its EgressPolicy names them, and the egress gateway puts
# them into requests after they leave the sandbox.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
context="${KUBE_CONTEXT:-kind-substrate}"
repo="${IMAGE_REPO:-localhost:5001}"
namespace=appa-substrate-demo
atespace=appa-substrate-demo
actor=claude-appa
secret=appa-substrate-demo-credentials

k() { kubectl --context "$context" "$@"; }
ate() { kubectl ate --context "$context" "$@"; }

if [[ "${1:-}" == "delete" ]]; then
  ate delete actor "$actor" -a "$atespace" 2>/dev/null || true
  ate delete actor-template "$actor" -a "$atespace" 2>/dev/null || true
  k delete namespace "$namespace" --ignore-not-found
  echo "Deleted the demo. The atespace and its namespace grant remain."
  exit 0
fi

: "${WORKER_IMAGE:?set WORKER_IMAGE to the installed ateworker-gvisor image}"
: "${ARCHESTRA_MCP_URL:?set ARCHESTRA_MCP_URL to an Archestra MCP gateway URL}"
: "${CLAUDE_TOKEN_FILE:?set CLAUDE_TOKEN_FILE to a file holding a claude setup-token token}"
: "${ARCHESTRA_TOKEN_FILE:?set ARCHESTRA_TOKEN_FILE to a file holding an Archestra token}"
ARCHESTRA_HOST="$(printf '%s' "$ARCHESTRA_MCP_URL" | sed -E 's#^https://([^/:]+).*#\1#')"
[[ "$ARCHESTRA_HOST" != "$ARCHESTRA_MCP_URL" ]] || {
  echo "ARCHESTRA_MCP_URL must be an https:// URL" >&2
  exit 1
}
export WORKER_IMAGE ARCHESTRA_MCP_URL ARCHESTRA_HOST

echo "==> Building the actor image"
image="$repo/appa-substrate-actor"
docker build -t "$image:latest" --build-arg "APPA_VERSION=${APPA_VERSION:-}" "$here/workload"
docker push "$image:latest" >/dev/null
digest="$(docker inspect --format '{{index .RepoDigests 0}}' "$image:latest")"
export ACTOR_IMAGE="$digest"
echo "    $ACTOR_IMAGE"

echo "==> Applying the namespace and worker pool"
envsubst '${WORKER_IMAGE}' <"$here/manifests/workerpool.yaml.tmpl" | k apply -f -

echo "==> Storing the credentials the egress gateway injects"
k -n "$namespace" create secret generic "$secret" \
  --from-file=claude-code-oauth-token=<(tr -d '[:space:]' <"$CLAUDE_TOKEN_FILE") \
  --from-file=archestra-token=<(tr -d '[:space:]' <"$ARCHESTRA_TOKEN_FILE") \
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
envsubst '${ACTOR_IMAGE} ${ARCHESTRA_MCP_URL} ${ARCHESTRA_HOST}' \
  <"$here/manifests/actor-template.yaml.tmpl" | ate create actor-template -f -
ate create actor "$actor" -a "$atespace" --template "$actor"
envsubst '${ARCHESTRA_HOST}' <"$here/manifests/egress-policy.yaml.tmpl" |
  ate create egress-policy "$actor" -a "$atespace" -f -

cat <<EOF

Done. Open the actor's console through Substrate's router:

  kubectl ate --context $context proxy actor $actor 8000:8080 -a $atespace
  open http://localhost:8000/

The router resumes the actor on the first request.
EOF
