#!/bin/sh
# Renders the demo chart under the values its templates guard against.
# Each case pins one behavior the chart README states: the defaults
# render, every agent carries the gate knob, a repeated agent name
# fails the render, the go cell's own names leave the set when the cell
# is off, and a scalar-looking name renders quoted wherever an object
# consumes it. Needs helm on PATH and no cluster. CI runs it
# (.github/workflows/ci.yml, kagent Chart Checks).
set -eu

chart=$(cd "$(dirname "$0")/.." && pwd)
app_version=$(sed -n 's/^appVersion: *"\([^"]*\)".*/\1/p' "$chart/Chart.yaml")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# render <namespace> [helm args...]: renders the chart into $work/out
# and $work/err, and returns helm's exit status.
render() {
  ns=$1
  shift
  helm template appa-kagent-demo "$chart" --namespace "$ns" "$@" >"$work/out" 2>"$work/err"
}

# must_render <namespace> [helm args...]: the render succeeds.
must_render() {
  if ! render "$@"; then
    echo "render failed: $*" >&2
    cat "$work/err" >&2
    exit 1
  fi
}

# must_refuse <substring> <namespace> [helm args...]: the render fails
# and its error names the substring.
must_refuse() {
  needle=$1
  shift
  if render "$@"; then
    echo "render succeeded, refusal expected: $*" >&2
    exit 1
  fi
  if ! grep -q -- "$needle" "$work/err"; then
    echo "render failed without '$needle': $*" >&2
    cat "$work/err" >&2
    exit 1
  fi
}

# count <pattern>: the number of rendered lines the extended regular
# expression matches.
count() {
  grep -c -E -- "$1" "$work/out" || true
}

# expect <n> <pattern>: exactly n rendered lines match the pattern.
expect() {
  found=$(count "$2")
  if [ "$found" -ne "$1" ]; then
    echo "expected $1 lines matching '$2', found $found" >&2
    exit 1
  fi
}

# expect_env <n> <name> <value>: exactly n rendered env entries name
# the variable and carry the value on the line below it.
expect_env() {
  found=$(grep -A 1 -- "- name: $2\$" "$work/out" | grep -c -- "value: \"$3\"\$" || true)
  if [ "$found" -ne "$1" ]; then
    echo "expected $1 env entries $2=$3, found $found" >&2
    exit 1
  fi
}

# The defaults render only demo-owned fixtures. appa-runtime owns the
# runtime, serving policy, persistence, provider configuration, and guide.
must_render kagent
expect 2 '^kind: Deployment$'
expect 2 '^kind: Service$'
expect 1 '^kind: RemoteMCPServer$'
expect 3 '^kind: Agent$'
expect 0 '^kind: PersistentVolumeClaim$'
expect 0 '^kind: ModelConfig$'
expect 0 '^kind: Secret$'
expect 0 '^  name: appa-runtime$'
expect 0 '^  name: appa-runtime-policy$'
expect 0 '^  name: appa-guide$'
expect 0 "image: europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/appa-runtime:v${app_version}$"
expect 0 '/opt/appa/batteries|/var/lib/appa|APPA_CONFIG|APPA_GUIDE_RUNTIME_URL'
expect 1 '^  name: appa-kagent-demo-policy$'
expect 1 '^    app.kubernetes.io/component: policy-template$'
expect 2 '^  name: appa-demo-mocks$'
expect 1 "image: europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/appa-demo-mocks:v${app_version}$"
expect 1 "image: europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/appa-demo-tools:v${app_version}$"
expect 1 '^        runAsNonRoot: true$'
expect 1 '^            readOnlyRootFilesystem: true$'
expect 5 'http://appa-demo-mocks\.kagent\.svc\.cluster\.local:8081/'
expect 1 '^    name = "skills"$'
expect 1 '^    name = "kagent__NS__log_analyst"$'
expect 1 '^    name = "kagent__NS__log_analyst_go"$'
expect 1 '^            - mcp__github__get_file_contents$'
expect 1 '^            - mcp__github__issue_write$'

# Every rendered Agent uses the one explicitly selected shared runtime.
expect_env 3 APPA_ENABLED true
expect_env 3 APPA_RUNTIME_URL http://appa-runtime.appa.svc.cluster.local:18787
expect 1 'never call ask_user'
must_render kagent --set-string runtime.url=https://policy.example.test
expect_env 3 APPA_RUNTIME_URL https://policy.example.test

# A name that repeats another agent name fails the render, the fixed
# cluster-ops and release-manager included.
must_refuse 'agent names collide' kagent --set agents.childName=release-manager
must_refuse 'agent names collide' kagent --set agents.go.enabled=true --set agents.childName=cluster-ops-go
must_refuse 'agent names collide' kagent --set agents.go.childName=log-analyst
must_refuse 'agent names collide' kagent --set agents.go.enabled=true --set agents.go.name=cluster-ops
must_refuse 'agent names collide' kagent --set agents.go.enabled=true --set agents.go.undeclaredName=release-manager

# Enabling the Go cell adds its three optional agents.
must_render kagent --set agents.go.enabled=true
expect 6 '^kind: Agent$'
expect_env 6 APPA_RUNTIME_URL http://appa-runtime.appa.svc.cluster.local:18787
expect 2 'never call ask_user'

must_render kagent --set agents.go.enabled=false --set agents.childName=release-manager-go
must_refuse 'agent names collide' kagent --set agents.go.enabled=false --set agents.childName=log-analyst-go

# A missing name fails the render before the policy renders.
must_refuse 'agents.childName is required' kagent --set agents.childName=null
must_refuse 'agents.go.childName is required' kagent --set agents.go.childName=null

# The schema refuses a name that is not a DNS-1123 label, and a name
# that is not a string.
must_refuse 'schema' kagent --set agents.childName=Log-Analyst
must_refuse 'schema' kagent --set agents.childName=123

# Scalar-looking names render quoted wherever an object consumes them.
must_render 123 --set-string agents.childName=123 --set-string agents.go.childName=null \
  --set agents.go.enabled=true --set-string modelConfig.name=123
expect 0 ': 123$'
expect 0 ': null$'
expect "$(count '^kind: ')" '^  namespace: "123"$'
# The child Agent and the parent's agent-tool reference.
expect 2 'name: "123"$'
# The go child Agent and the go parent's agent-tool reference.
expect 2 'name: "null"$'
expect 6 '^    modelConfig: "123"$'
expect 1 '^    name = "123__NS__123"$'
expect 1 '^    name = "123__NS__null"$'

# Required external references fail before Kubernetes sees an unusable Agent.
must_refuse "missing property 'url'" kagent --set runtime.url=null
must_refuse 'schema' kagent --set-string runtime.url=appa-runtime.appa:18787
must_refuse 'schema' kagent --set-string modelConfig.name=Default

# Removed ownership knobs fail loudly instead of becoming ignored values.
must_refuse "additional properties 'guide' not allowed" kagent --set guide.enabled=false
must_refuse "additional properties 'openai' not allowed" kagent --set-string openai.apiKey=placeholder
must_refuse "additional properties 'image' not allowed" kagent --set runtime.image.repository=example.invalid/runtime

echo "render-test: every case passed"
