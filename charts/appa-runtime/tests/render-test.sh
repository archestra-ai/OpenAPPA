#!/bin/sh
# Renders the shared runtime chart under the values its templates guard.
# Needs helm on PATH and no cluster.
set -eu

chart=$(cd "$(dirname "$0")/.." && pwd)
app_version=$(sed -n 's/^appVersion: *"\([^"]*\)".*/\1/p' "$chart/Chart.yaml")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

render() {
  helm template appa-runtime "$chart" --namespace appa "$@" >"$work/out" 2>"$work/err"
}

must_render() {
  if ! render "$@"; then
    echo "render failed: $*" >&2
    cat "$work/err" >&2
    exit 1
  fi
}

count() {
  grep -c -E -- "$1" "$work/out" || true
}

expect() {
  found=$(count "$2")
  if [ "$found" -ne "$1" ]; then
    echo "expected $1 lines matching '$2', found $found" >&2
    exit 1
  fi
}

must_contain() {
  if ! grep -F -q -- "$1" "$work/out"; then
    echo "render missing '$1'" >&2
    exit 1
  fi
}

must_not_contain() {
  if grep -F -q -- "$1" "$work/out"; then
    echo "render has unexpected '$1'" >&2
    exit 1
  fi
}

must_render
expect 1 '^kind: Deployment$'
expect 1 '^kind: Service$'
expect 1 '^kind: ServiceAccount$'
must_contain 'image: ghcr.io/archestra-ai/appa-runtime:'
must_contain '--batteries-dir'
must_contain '/opt/appa/batteries'
must_contain 'containerPort: 18787'
must_contain 'targetPort: runtime'
must_contain 'port: 18787'
must_contain 'port: runtime'
must_contain '0.0.0.0:18787'
must_contain 'appa-runtime.appa.svc.cluster.local:18787'
must_not_contain 'relay'
must_not_contain 'nginx-unprivileged'
must_not_contain '18789'
must_not_contain '/var/lib/appa/batteries'
must_not_contain '/var/lib/appa/release-batteries'
must_not_contain 'kind: PersistentVolumeClaim'
must_not_contain 'kind: NetworkPolicy'
must_not_contain 'kind: Agent'
must_contain 'emptyDir: {}'
must_contain 'runAsNonRoot: true'
must_contain 'runAsUser: 65532'
must_contain 'readOnlyRootFilesystem: true'
must_contain 'checksum/policy:'
must_contain 'name: APPA_CONFIG'
must_contain 'value: "/etc/appa/appa.toml"'
must_contain 'mountPath: /var/run/appa/identity'
must_contain 'path: pod-name'
must_contain 'fieldPath: metadata.namespace'
expect 1 '^          readinessProbe:$'
expect 1 '^          livenessProbe:$'
expect 1 '^          startupProbe:$'

must_render --set appaGuide.enabled=true
expect 1 '^kind: Agent$'
must_contain 'name: appa-guide'
must_contain 'namespace: "kagent"'
must_contain "ref: \"v${app_version}\""
must_contain 'http://appa-runtime.appa.svc.cluster.local:18787'
must_contain 'name: "kagent-tool-server"'
must_contain 'your first tool call must be skills with command appa-guide'
must_contain 'Inventory Agents as JSON and RemoteMCPServers with a wide list'
must_contain 'immediately call execute_remedy_plan with its exact offer id'
must_contain 'copy pod_name and namespace from the same fetched Pod YAML'
must_contain 'with k8s_apply_manifest over its complete observed spec'
must_contain 'A request to protect an Agent'
must_contain 'Never copy status'
must_contain 'request itself'
must_contain 'available, not included'

must_render --set appaGuide.enabled=true --set appaGuide.namespace=platform \
  --set appaGuide.skill.ref=main --set appaGuide.modelConfig=platform-model
must_contain 'namespace: "platform"'
must_contain 'ref: "main"'
must_contain 'modelConfig: "platform-model"'

must_render --set persistence.enabled=true --set persistence.size=10Gi
must_contain 'kind: PersistentVolumeClaim'
must_contain '/var/lib/appa/batteries'
must_contain '/var/lib/appa/release-batteries'
must_contain 'name: recover-battery-refresh'
must_contain '.release-batteries.previous'
must_contain 'storage: "10Gi"'
must_contain 'helm.sh/resource-policy: keep'
must_contain 'claimName:'

must_render --set persistence.enabled=true --set persistence.existingClaim=team-appa
must_contain 'claimName: team-appa'
must_not_contain 'kind: PersistentVolumeClaim'

must_render --set config.existingConfigMap=my-policy --set config.key=policy.toml
must_contain 'name: my-policy'
must_contain '/etc/appa/policy.toml'
must_not_contain 'name: appa-runtime-policy'
must_not_contain 'checksum/policy:'

printf '%s\n' 'include = ["batteries/slack/appa.toml"]' >"$work/policy.toml"
must_render --set-file config.contents="$work/policy.toml"
must_contain 'include = ["batteries/slack/appa.toml"]'

must_contain 'busybox:1.37@sha256:9db7b59979c38555a39def84a31fb98b5296952f9e3afd4f6f11f05b07adfab0'

if render --set env.APPA_CONFIG=/tmp/other; then
  echo "render accepted reserved APPA_CONFIG" >&2
  exit 1
fi
if ! grep -F -q 'env.APPA_BATTERIES_DIR and env.APPA_CONFIG are reserved' "$work/err"; then
  echo "reserved env refusal did not name the contract" >&2
  exit 1
fi

if render --set networkPolicy.enabled=true; then
  echo "render accepted an enabled NetworkPolicy without peers" >&2
  exit 1
fi
must_render --set networkPolicy.enabled=true \
  --set 'networkPolicy.ingress[0].podSelector.matchLabels.app=kagent-agent'
must_contain 'kind: NetworkPolicy'
must_contain 'app: kagent-agent'
must_contain 'port: runtime'

echo "render-test: every case passed"
