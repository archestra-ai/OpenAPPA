# appa-runtime

Shared OpenAPPA runtime for a Kubernetes cluster. One replica. Agents
that set `APPA_RUNTIME_URL` to this Service, with `APPA_ENABLED=true`,
are gated by the policy in the ConfigMap.

The image of this chart version is `ghcr.io/archestra-ai/appa-runtime:0.9.0`. # x-release-please-version

## Install

Install a released chart from GHCR after setting `APPA_VERSION` to an
OpenAPPA release that contains the chart:

```sh
helm install appa-runtime oci://ghcr.io/archestra-ai/charts/appa-runtime \
  --version "$APPA_VERSION" --namespace appa --create-namespace
```

Install from a repository checkout during development:

```sh
helm install appa-runtime charts/appa-runtime --namespace appa --create-namespace
```

An unreleased checkout can name an image tag that is not published yet.
Set `image.repository` and `image.tag` to the image built from that
checkout.

GHCR packages start private. An organization owner must make the
`charts/appa-runtime` package public after its first publish before an
anonymous OCI install can pull it.

The runtime binds loopback inside the pod. A relay sidecar is the
cluster address. Point agents at:

```text
http://appa-runtime.appa.svc.cluster.local:18789
```

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

The chart mounts a fail-closed policy. The appa-guide skill on kagent
adds batteries and root rules through the ConfigMap. Set
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

Without a NetworkPolicy, any pod that can reach the Service can call
`/hook`, `/mcp`, `/health`, and `/batteries`. Administrative routes stay
loopback-only. Restrict callers by enabling the chart policy and listing
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
