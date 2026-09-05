# The live A2A matrix on kind

Three scripts stand the demo stack up on a kind cluster and run all 18
A2A matrix cases against a real model. CI gates on them after three
attempts per case, and the same three commands run on a laptop or a dev
VM. The other suites in [../](../) need a stack that already runs, or no
cluster at all. This directory is the one place that creates one.

These are the A2A driver on cell A-py, the declarative Python agent on
kagent v0.9.12. The dashboard driver and Go cell stay with
[../run-matrix.sh](../run-matrix.sh) against a stack you keep.

## Run it

Build the four images at the tag the scripts load, from the repository
root:

```sh
docker build -f appa-runtime/Dockerfile -t appa-runtime:ci .
docker build -t appa-kagent-adk:ci integrations/kagent/appa-kagent-adk
docker build -t appa-demo-tools:ci integrations/kagent/demo
docker build -t appa-demo-mocks:ci integrations/kagent/demo/mocks
```

Then, from this directory:

```sh
./kind-up.sh                              # the cluster, and the images into it
OPENROUTER_API_KEY=… ./install.sh         # kagent, runtime chart, fixture chart
./run-a2a.sh                              # all 18 A2A cases
```

`kind-up.sh` keeps a cluster that already carries the name and leaves it
as the current kubectl context. `install.sh` installs kagent with the
agent image pointed at the loaded `appa-kagent-adk:ci` and turns off
the UI, ten sample agents, and three bundled tool charts. It creates an
e2e ModelConfig, installs the production runtime chart with the rendered
demo policy, then installs the fixture-only demo chart without the Go
cell or seed Job. It waits for `appa-runtime`, both demo Deployments, and
the three Agent Deployments the kagent controller compiles.
`run-a2a.sh` port-forwards the parent Agent and the mocks, runs every
case with two reruns, and exits with pytest's status.

Each script reads its settings from the environment
(`APPA_E2E_NAMESPACE`, `APPA_E2E_RUNTIME_NAMESPACE`,
`APPA_E2E_IMAGE_TAG`, `APPA_E2E_MODEL`, `APPA_E2E_BASE_URL`, and the
rest at the top of each file). `KIND_CLUSTER` names the cluster, `KAGENT_VERSION` the kagent
chart. `APPA_E2E_PRUNE_DAEMON_IMAGES=1` drops the daemon copies of the
images after the load, which a 14 GB CI runner needs.

## The model

One key and endpoint serve the Agents through the e2e ModelConfig. The
demo sanitizers are deterministic mock policy services and consume no
model credential. The default Agent model matches the public playground:
`openai/gpt-5.6-luna` through OpenRouter.

The Agent model must support function calling. Sanitizer support and
structured model output are not prerequisites for this matrix.

## In CI

The workflow job builds the four images, then runs these same three
scripts. It runs only when a maintainer adds the `run-e2e` label to the pull
request and the changed paths touch the stack. Fork pull requests never
run it, because it needs `OPENROUTER_API_KEY`. Remove the label and add
it again to re-run it after new commits. Every test gets three total
attempts; a test that exhausts them fails the job. `run-a2a.sh` appends the
model, the endpoint, the matrix size and the wall time to the job
summary.
