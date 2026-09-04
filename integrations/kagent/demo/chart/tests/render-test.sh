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

# The defaults render the Python cell and the guide agent, and the policy
# declares both children by their wire spelling.
must_render kagent
expect 2 '/opt/appa/batteries'
expect 0 '/var/lib/appa/batteries'
expect 1 'image: ghcr.io/archestra-ai/appa-runtime:0\.10\.0$'
expect 4 '^kind: Agent$'
expect_env 1 APPA_CONFIG /etc/appa/demo.appa.toml
expect 1 '^  name: appa-guide$'
expect 1 '/skills/appa-guide/references/kagent\.md'
expect 1 'with offset 1 and limit 0. Follow'
expect 1 'all read-only inspection and present the proposal without asking whether'
expect 1 'your first tool call must be skills'
expect 1 'selecting app=appa-runtime; never construct its'
expect 1 'until the full init checklist and comparison are complete'
expect 1 '^            - name: APPA_GUIDE_RUNTIME_URL$'
expect 1 '^    name = "skills"$'
expect 1 '^    name = "kagent__NS__log_analyst"$'
expect 1 '^    name = "kagent__NS__log_analyst_go"$'

# Persistence adds the writable lookup path. An existing claim is used
# without rendering a second claim.
must_render kagent --set runtime.persistence.enabled=true \
  --set runtime.persistence.existingClaim=team-appa
expect 2 '/var/lib/appa/batteries'
expect 2 '/var/lib/appa/release-batteries'
expect 1 'claimName: "team-appa"'
expect 0 '^kind: PersistentVolumeClaim$'

# Every rendered agent carries the gate knob beside the runtime URL.
# The runtime image is a drop-in replacement for the stock kagent image
# and runs ungated until APPA_ENABLED reads true, and this is a gated
# demo, so every agent sets it.
expect_env 4 APPA_ENABLED true
expect 4 '^        - name: APPA_RUNTIME_URL$'

# A name that repeats another agent name fails the render, the fixed
# cluster-ops and release-manager included.
must_refuse 'agent names collide' kagent --set agents.childName=release-manager
must_refuse 'agent names collide' kagent --set agents.go.enabled=true --set agents.childName=cluster-ops-go
must_refuse 'agent names collide' kagent --set agents.go.childName=log-analyst
must_refuse 'agent names collide' kagent --set agents.go.enabled=true --set agents.go.name=cluster-ops
must_refuse 'agent names collide' kagent --set agents.go.enabled=true --set agents.go.undeclaredName=release-manager

# Enabling the Go cell adds its three optional agents.
must_render kagent --set agents.go.enabled=true
expect 7 '^kind: Agent$'

# The guide agent is its own switch, and it leaves the collision set: it
# takes no value-derived name.
must_render kagent --set guide.enabled=false
expect 3 '^kind: Agent$'
expect 0 '^  name: appa-guide$'
must_render kagent --set agents.go.enabled=false --set agents.childName=release-manager-go
must_refuse 'agent names collide' kagent --set agents.go.enabled=false --set agents.childName=log-analyst-go

# A missing name fails the render before the policy renders.
must_refuse 'agents.childName is required' kagent --set agents.childName=null
must_refuse 'agents.go.childName is required' kagent --set agents.go.childName=null

# The schema refuses a name that is not a DNS-1123 label, and a name
# that is not a string.
must_refuse 'schema' kagent --set agents.childName=Log-Analyst
must_refuse 'schema' kagent --set agents.childName=123

# Scalar-looking names render quoted wherever an object consumes them:
# a numeric release namespace, a numeric ModelConfig name (an integer
# after --set), and children named 123 and null.
must_render 123 --set-string agents.childName=123 --set-string agents.go.childName=null \
  --set agents.go.enabled=true --set modelConfig.name=123 --set openai.apiKey=k
expect 0 ': 123$'
expect 0 ': null$'
expect "$(count '^kind: ')" '^  namespace: "123"$'
# The child Agent, the Secret, the ModelConfig, the parent's agent-tool
# reference and the runtime's secretKeyRef.
expect 5 'name: "123"$'
# The go child Agent and the go parent's agent-tool reference.
expect 2 'name: "null"$'
expect 7 '^    modelConfig: "123"$'
expect 1 '^  apiKeySecret: "123"$'
expect 1 '^    name = "123__NS__123"$'
expect 1 '^    name = "123__NS__null"$'

# The model profile the policy's sanitizers consult. Defaults use OpenAI's
# endpoint. One override reaches both the agents' ModelConfig and that profile.
must_render kagent
expect 1 '^    model = "gpt-4.1-mini"$'
expect 0 '^    url = "https://openrouter.ai/api/v1"$'
expect 0 '^    baseUrl: '
must_render kagent --set-string openai.baseUrl=https://openrouter.ai/api/v1 \
  --set-string llm.url=https://openrouter.ai/api/v1 --set-string llm.model=vendor/model:free
expect 1 '^    baseUrl: "https://openrouter.ai/api/v1"$'
expect 1 '^    url = "https://openrouter.ai/api/v1"$'
expect 1 '^    model = "vendor/model:free"$'

# A missing sanitizer model fails the render.
must_refuse 'llm.model is required' kagent --set llm.model=null

echo "render-test: every case passed"
