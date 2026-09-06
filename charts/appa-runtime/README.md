# appa-runtime

Shared OpenAPPA runtime for a Kubernetes cluster. One replica. Agents
that set `APPA_RUNTIME_URL` to this Service, with `APPA_ENABLED=true`,
are gated by the policy in the ConfigMap.

The image of this chart version is `europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/appa-runtime:v0.12.0`. # x-release-please-version

## Install

Install a released chart from Artifact Registry after setting
`APPA_VERSION` to an OpenAPPA release that contains the chart:

```sh
helm install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" --namespace appa --create-namespace
```

Install from a repository checkout during development:

```sh
helm install appa-runtime charts/appa-runtime --namespace appa --create-namespace
```

An unreleased checkout can name an image tag that is not published yet.
Set `image.repository` and `image.tag` to the image built from that
checkout.

## appa-guide

The chart can install the configuring kagent Agent with the runtime:

```sh
helm install appa-runtime oci://europe-west1-docker.pkg.dev/friendly-path-465518-r6/appa-public/charts/appa-runtime \
  --version "$APPA_VERSION" --namespace appa --create-namespace \
  --set appaGuide.enabled=true \
  --set appaGuide.namespace=kagent
```

The option is off by default because the runtime chart also supports
clusters without kagent. The target namespace, kagent model config, and
tool-server name are configurable under `appaGuide`. An empty
`appaGuide.skill.ref` pins the skill to the chart's `v<appVersion>` tag.

Enabling `appaGuide` opens a separate guide MCP listener on Service port
`18788`. Only the `appa-guide` Agent receives that URL and management
toolset. The chart adds a NetworkPolicy that permits that port only from
the labeled guide pod, plus a Role allowing the runtime ServiceAccount to
get and patch its one policy ConfigMap. Every guide MCP call also consumes
a one-shot APPA vouch, so direct calls cannot read or mutate management
state. The normal `/mcp` endpoint remains remedy-only.

The runtime binds the pod network directly. Point agents at:

```text
http://appa-runtime.appa.svc.cluster.local:18787
```

## Demo fixtures

`appa-kagent-demo` is a separate fixture-only chart. It points its Agents
at this Service and supplies an inert policy template for `appa-guide` to
review. It does not install a runtime, serving policy, persistence,
provider configuration, ModelConfig, or second `appa-guide`.

## Persistence

Off by default. The trajectory log and a writable batteries overlay
then live on emptyDir and die with the pod.

```sh
helm upgrade appa-runtime charts/appa-runtime --namespace appa \
  --set persistence.enabled=true \
  --set persistence.size=8Gi
```

Use an existing claim:

```sh
helm upgrade appa-runtime charts/appa-runtime --namespace appa \
  --set persistence.enabled=true \
  --set persistence.existingClaim=appa-data
```

With persistence on, the runtime uses this lookup order:

1. `/var/lib/appa/batteries` for the operator-managed overlay.
2. `/var/lib/appa/release-batteries` for the latest verified release.
3. `/opt/appa/batteries` for the batteries built into the running image.

The appa-guide skill can refresh the second directory from the latest
published semver release after approval. It verifies the plugin archive
against that release's `SHA256SUMS` and validates the serving root
config. It retains the previous layer until policy reload succeeds, then
commits the refresh. A refused reload rolls the layer back. The operator
overlay stays unchanged. Without a persistent volume the
skill does not refresh either directory.

If a process stops between the two directory renames, the chart's init
container restores `.release-batteries.previous` before the runtime
starts. The prior layer remains on the PVC throughout the transaction.

## Policy

The chart mounts a bootstrap policy that lets appa-guide inspect the
cluster and requires human approval for policy management. Every
unrelated tool remains fail-closed. Typed runtime MCP tools validate,
publish, reload, and roll back policy and battery changes. Set
`config.existingConfigMap` to manage that ConfigMap yourself.

When `config.contents` stays empty, an upgrade preserves the live policy
key that appa-guide changed. Setting `config.contents` explicitly makes
Helm replace that key. An existing ConfigMap always remains under the
operator's ownership.

Keep `config.key` unchanged after appa-guide manages that key. Helm
treats another key as a new policy and initializes it from chart values.

Live preservation requires a cluster-connected `helm upgrade`, because
it reads the ConfigMap with Helm `lookup`. Template-only GitOps renderers
cannot see live state. Set `config.contents` or `config.existingConfigMap`
when Argo CD or another template-only renderer owns the release.

Do not run `helm upgrade` while appa-guide applies a policy. Helm reads
the live key during template rendering; a concurrent later write wins.

## Network access

Without the optional general NetworkPolicy, any pod that can reach the
Service can call `/hook`, remedy-only `/mcp`, `/health`, and `/batteries`.
When appa-guide is enabled, its dedicated NetworkPolicy restricts port
`18788` to the guide pod. A one-shot vouch still refuses a direct
`/guide-mcp` call that never passed a gated ToolCall. `/hook` is
unauthenticated. A client that can reach both `/hook` and `/guide-mcp`
can complete the gated approval path. Enable a CNI that enforces
NetworkPolicy, or the optional general NetworkPolicy. The vouch is not a
substitute for that network boundary. The runtime returns `403` on
`/reload`, `/status`, `/policy-key`, and `/binary-fingerprint` unless the
network peer is loopback. Treat the Service as trusted internal
infrastructure. Restrict callers by enabling the chart policy and listing
Kubernetes `NetworkPolicyPeer` objects:

```yaml
networkPolicy:
  enabled: true
  ingress:
    - namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: kagent
```

The chart refuses an enabled policy with no peers.

## Batteries

`GET /batteries` lists the batteries this image (and overlay) can
include. An `include = ["batteries/<name>/appa.toml"]` line in the
root policy resolves against those directories. Root rules still run
first.
