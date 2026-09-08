# Marketplace kagent acceptance

This lane runs the installed GitHub battery through ordinary kagent 0.9.12
Python and Go Agents. A scripted OpenAI-compatible HTTP service replaces only
the external model. A local MCP service records real tool calls. There are no
provider credentials, real GitHub writes, or patched kagent model factories.

The CI job builds four images, pushes them to an isolated local registry, records
actual OCI digests, and packages a test-only offline deployment. The test drives
the real plugin/battery CLI and deploys its resulting chart and settings into a
uniquely named amd64 kind cluster, using a dedicated kubeconfig throughout.

It checks registry and running-image digests, rejects missing running evidence,
and exercises three cases for each language:

- Refuse a suspicious repository read before the MCP handler runs.
- Accept the offered trust change, read the repository, then refuse a public write.
- Allow operator-authored public text in a fresh trajectory.

Assertions check actual MCP invocation counts and model-visible policy feedback,
not just the final model text. Then the lane exports/imports the deployment,
stops its registry, redeploys the runtime, restarts both Agents from cached images,
and repeats the checks. This demonstrates offline marketplace restore and cached
image reuse, not isolation from every possible network destination.

Run on an amd64 Docker host with cargo, helm, kubectl, kind and crane available:

```sh
docker build -f appa-runtime/Dockerfile -t appa-acceptance-runtime:ci .
docker build -t appa-acceptance-python:ci integrations/kagent/appa-kagent-adk
docker build -t appa-acceptance-go:ci integrations/kagent/appa-kagent-adk-go
docker build -t appa-acceptance-fixtures:ci integrations/kagent/e2e/marketplace
python3 integrations/kagent/e2e/marketplace/run.py /tmp/new-acceptance-directory
```

The output directory must not exist. The script deletes only its own named
cluster and registry container, keeps logs and evidence, and never switches the
user's current context. Do not publish its kubeconfig. All generated generations
are fixtures; unused native Claude/binary descriptor entries are placeholders,
not release artifacts. Successful fixture unit tests alone are not cluster proof.
