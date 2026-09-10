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

must_refuse() {
  reason=$1
  shift
  if render "$@"; then
    echo "render unexpectedly accepted: $*" >&2
    exit 1
  fi
  if [ -n "$reason" ] && ! grep -F -q -- "$reason" "$work/err"; then
    echo "render refusal did not contain '$reason'" >&2
    cat "$work/err" >&2
    exit 1
  fi
}

must_render
expect 1 '^kind: Deployment$'
expect 1 '^kind: Service$'
expect 1 '^kind: ServiceAccount$'
must_contain "image: europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/appa-runtime:v${app_version}"
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
must_not_contain 'kind: Role'
must_not_contain 'kind: RoleBinding'
must_contain 'automountServiceAccountToken: false'
must_contain 'emptyDir: {}'
must_contain 'runAsNonRoot: true'
must_contain 'runAsUser: 65532'
must_contain 'readOnlyRootFilesystem: true'
must_contain 'checksum/policy:'
must_contain 'appa.dev/packaged-policy-sha256:'
must_contain 'annotator = "appa-guide-apply"'
must_not_contain 'appa-guide-command'
must_not_contain 'k8s_execute_command'
must_contain 'name: APPA_CONFIG'
must_contain 'value: "/etc/appa/appa.toml"'
must_contain 'workingDir: /etc/appa'
must_contain 'mode: 0444'
must_contain 'name: APPA_GUIDE_RUNTIME_URL'
must_contain 'value: "http://127.0.0.1:18787"'
must_contain 'name: APPA_PERSISTENCE_ENABLED'
must_contain 'value: "false"'
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
must_contain 'first call skills with'
must_contain 'Runtime management uses only direct runtime-owned MCP tools'
must_contain 'appa_get_runtime_state reads serving policy'
must_contain 'appa_include_battery updates the complete root policy and reloads it'
must_contain 'appa_update_policy publishes one complete approved root policy and reloads it'
must_contain 'appa_reload_policy reloads the mounted complete root policy'
must_contain 'appa_refresh_batteries refreshes, validates, reloads'
must_contain 'Never use Kubernetes tools, shell commands, helper executables'
must_contain 'Pass the policy key from appa_get_runtime_state'
must_contain 'never Helm values or Kubernetes Secrets'
must_contain 'APPA_ENABLED alone does'
must_contain 'matches, included, and unconfigured_tools fields'
must_contain 'Never call appa_include_battery'
must_contain 'serving policy must declare'
must_contain 'ascending discovered-tool count'
must_contain 'release-manager'
must_contain 'remains a blocked delegation'
must_contain 'A request is never approval'
must_contain 'Never invent or request an offer id'
must_contain 'Protect an existing Agent only with k8s_apply_manifest'
must_contain 'Never patch a generated Deployment'
must_contain 'If the request says diagnose and inspect only'
must_contain 'No changes applied'
must_contain 'under'
must_contain '1,600 characters'
must_not_contain '- k8s_execute_command'
must_not_contain '- k8s_patch_resource'
must_not_contain '- k8s_get_events'
must_not_contain '- k8s_get_pod_logs'
expect 1 '^kind: Role$'
expect 1 '^kind: RoleBinding$'
expect 1 '^kind: NetworkPolicy$'
must_contain 'resourceNames: ["appa-runtime-policy"]'
must_contain 'verbs: ["get", "patch"]'
must_contain 'automountServiceAccountToken: true'
must_contain 'name: APPA_GUIDE'
must_contain 'name: APPA_GUIDE_MCP_URL'
must_contain 'name: APPA_KAGENT_OPENAI_REASONING_EFFORT'
must_contain 'http://appa-runtime.appa.svc.cluster.local:18788/guide-mcp'
must_contain 'name: guide-mcp'
must_contain 'port: 18788'
must_contain 'app.kubernetes.io/name: "appa-guide"'

# An upgrade with --reuse-values from a release predating guidePort keeps
# the isolated management endpoint on its stable default.
must_render --set appaGuide.enabled=true --set service.guidePort=null
must_contain 'port: 18788'
must_contain 'containerPort: 18788'

must_render --set appaGuide.enabled=true --set appaGuide.namespace=platform \
  --set appaGuide.skill.ref=main --set appaGuide.modelConfig=platform-model \
  --set-string appaGuide.reasoningEffort=none
must_contain 'namespace: "platform"'
must_contain 'ref: "main"'
must_contain 'modelConfig: "platform-model"'
must_contain 'value: "none"'

must_refuse 'appaGuide.skill.ref must be a branch or tag' --set appaGuide.enabled=true \
  --set-string appaGuide.skill.ref=9b46eeffedee4b0a1f00dd967fc3eba9c856db8d
must_refuse 'appaGuide.skill.ref must be a branch or tag' --set appaGuide.enabled=true \
  --set-string appaGuide.skill.ref=9b46eef
must_render --set appaGuide.enabled=true --set-string appaGuide.skill.ref=v0.16.0
must_contain 'ref: "v0.16.0"'

must_render --set persistence.enabled=true --set persistence.size=10Gi
must_contain 'kind: PersistentVolumeClaim'
must_contain '/var/lib/appa/batteries'
must_contain '/var/lib/appa/release-batteries'
must_contain 'name: recover-battery-refresh'
must_contain '.release-batteries.previous'
must_contain 'storage: "10Gi"'
must_contain 'helm.sh/resource-policy: keep'
must_contain 'claimName:'
must_contain 'value: "true"'

must_render --set persistence.enabled=true --set persistence.existingClaim=team-appa
must_contain 'claimName: team-appa'
must_not_contain 'kind: PersistentVolumeClaim'

must_render --set config.existingConfigMap=my-policy --set config.key=policy.toml
must_contain 'name: my-policy'
must_contain '/etc/appa/policy.toml'
must_contain 'name: APPA_POLICY_CONFIGMAP_NAME'
must_contain 'name: APPA_POLICY_CONFIGMAP_KEY'
must_contain 'name: APPA_RUNTIME_RELEASE_NAME'
must_not_contain 'name: appa-runtime-policy'
must_not_contain 'checksum/policy:'
must_not_contain 'appa.dev/packaged-policy-sha256:'

# Complete small trees preserve relative paths and executable intent. Hashed
# ConfigMap keys do not leak into the mounted filenames.
tree='{"rules/policy.toml":{"data":"YWJj","executable":false},".appa/helpers/run":{"data":"IyEvYmluL3NoCg==","executable":true},"empty":{"data":"","executable":false}}'
must_render --set-json "config.files=$tree"
must_contain 'binaryData:'
must_contain 'path: "rules/policy.toml"'
must_contain 'path: ".appa/helpers/run"'
must_contain 'path: "empty"'
must_contain 'mode: 0555'
expect 3 '^  "[a-f0-9]{64}":'
checksum=$(grep 'checksum/policy:' "$work/out")
must_render --set-json 'config.files={"rules/policy.toml":{"data":"YWJj","executable":true}}'
other_checksum=$(grep 'checksum/policy:' "$work/out")
[ "$checksum" != "$other_checksum" ] || { echo 'files did not affect checksum' >&2; exit 1; }
checksum=$other_checksum
must_render --set-json 'config.files={"rules/policy.toml":{"data":"YWJj","executable":false}}'
[ "$checksum" != "$(grep 'checksum/policy:' "$work/out")" ] || { echo 'mode did not affect checksum' >&2; exit 1; }

# A large tree is populated outside Helm and mounted read-only. This is not
# the writable trajectory-data claim and does not create or populate a PVC.
must_render --set config.existingClaim=prepared-tree --set config.key=policy.toml
must_contain 'claimName: "prepared-tree"'
must_contain 'readOnly: true'
must_contain '/etc/appa/policy.toml'
must_not_contain 'kind: ConfigMap'
must_not_contain 'kind: PersistentVolumeClaim'
must_not_contain 'checksum/policy:'
must_refuse 'mutually exclusive' --set config.existingClaim=tree --set config.contents=policy
must_refuse 'mutually exclusive' --set config.existingClaim=tree --set config.existingConfigMap=policy
must_refuse 'mutually exclusive' --set config.existingClaim=tree --set-json "config.files=$tree"
must_refuse 'appaGuide requires a policy ConfigMap' --set config.existingClaim=tree --set appaGuide.enabled=true
must_refuse 'requires a chart-managed ConfigMap' --set config.existingConfigMap=policy --set-json "config.files=$tree"

for path in '' '/absolute' 'a//b' 'a/' '.' '../x' 'a/./b' 'a/../b' '..data/x' 'a/..reserved' 'C:/x' 'a:b'; do
  must_refuse 'not a safe relative path' --set-json "config.files={\"$path\":{\"data\":\"\",\"executable\":false}}"
done
must_refuse 'not a safe relative path' --set-json 'config.files={"a\\b":{"data":"","executable":false}}'
must_refuse 'not a safe relative path' --set-json 'config.files={"a\nb":{"data":"","executable":false}}'
must_refuse 'collides with config.key' --set-json 'config.files={"appa.toml":{"data":"","executable":false}}'
must_refuse 'collides with a generated config.files ConfigMap key' \
  --set-string config.key=2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881 \
  --set-json 'config.files={"x":{"data":"","executable":false}}'
must_refuse 'has a file as its parent' --set-json 'config.files={"appa.toml/x":{"data":"","executable":false}}'
must_refuse 'has a file as its parent' --set-json 'config.files={"a":{"data":"","executable":false},"a/b":{"data":"","executable":false}}'
for data in '!' 'YQ' 'YQ===' 'YR=='; do
  must_refuse 'must be canonical base64' --set-json "config.files={\"x\":{\"data\":\"$data\",\"executable\":false}}"
done
must_refuse 'config.key must be a single safe ConfigMap key' --set-string config.key=../root
# Helm versions use different JSON Schema validators and diagnostic prose.
# These cases assert the contract (refusal), not a validator's wording.
must_refuse '' --set-json 'config.files={"x":{"data":42,"executable":false}}'
must_refuse '' --set-json 'config.files={"x":{"data":"","executable":"false"}}'
must_refuse '' --set-json 'config.files={"x":{"data":""}}'
# Valid canonical base64 exceeding the encoded budget (without huge argv).
awk 'BEGIN { printf "config:\n  files:\n    large:\n      executable: false\n      data: \""; for (i=0; i<192000; i++) printf "AAAA"; print "\"" }' >"$work/large.yaml"
must_refuse 'exceeds the 750 KiB encoded budget' -f "$work/large.yaml"

printf '%s\n' 'include = ["batteries/slack/appa.toml"]' >"$work/policy.toml"
must_render --set-file config.contents="$work/policy.toml"
must_contain 'include = ["batteries/slack/appa.toml"]'

must_contain 'busybox:1.37@sha256:9db7b59979c38555a39def84a31fb98b5296952f9e3afd4f6f11f05b07adfab0'

if render --set env.APPA_CONFIG=/tmp/other; then
  echo "render accepted reserved APPA_CONFIG" >&2
  exit 1
fi
if ! grep -F -q 'env.APPA_BATTERIES_DIR, env.APPA_CONFIG, env.APPA_GUIDE_RUNTIME_URL, env.APPA_PERSISTENCE_ENABLED, env.APPA_POLICY_CONFIGMAP_NAME, env.APPA_POLICY_CONFIGMAP_KEY, env.APPA_RUNTIME_RELEASE_NAME, APPA_PROXY_TOKEN, and APPA_PROXY_APPROVAL_SECRET are reserved' "$work/err"; then
  echo "reserved env refusal did not name the contract" >&2
  exit 1
fi

if render --set proxyTokenSecret.name=shared --set proxyTokenSecret.key=token \
  --set proxyApprovalSecret.name=shared --set proxyApprovalSecret.key=token; then
  echo "render accepted identical proxy Secret references" >&2
  exit 1
fi
if ! grep -F -q 'proxyTokenSecret and proxyApprovalSecret must not reference the same Secret key' "$work/err"; then
  echo "proxy Secret reference refusal did not name the contract" >&2
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

must_render --set appaGuide.enabled=true --set networkPolicy.enabled=true \
  --set 'networkPolicy.ingress[0].podSelector.matchLabels.app=kagent-agent'
expect 1 '^kind: NetworkPolicy$'
must_contain 'app.kubernetes.io/name: "appa-guide"'
must_contain 'port: runtime'
must_contain 'port: guide-mcp'

echo "render-test: every case passed"
