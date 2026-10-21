# Kubernetes battery

Rules for the Kubernetes MCP server
[`containers/kubernetes-mcp-server`](https://github.com/containers/kubernetes-mcp-server):
its default `config` and `core` toolsets and the optional `helm` toolset.
Plain TOML rules, no helper process and no provider credential of its own.
The server reaches the cluster with its own kubeconfig or in-cluster
ServiceAccount. One namespace, `kubernetes`, is bound to the server:

```sh
appa battery install kubernetes --server <kubernetes-server>
```

## Server version

The rules follow release
[`v0.0.67`](https://github.com/containers/kubernetes-mcp-server/releases/tag/v0.0.67),
commit `fcd9bbe672c98bb81c2ce93da8c0add548023672`. Each toolset registers
its tools in one directory:

- [`pkg/toolsets/config/configuration.go`](https://github.com/containers/kubernetes-mcp-server/blob/fcd9bbe672c98bb81c2ce93da8c0add548023672/pkg/toolsets/config/configuration.go)
  (3 tools);
- [`pkg/toolsets/core`](https://github.com/containers/kubernetes-mcp-server/tree/fcd9bbe672c98bb81c2ce93da8c0add548023672/pkg/toolsets/core):
  `events.go`, `namespaces.go`, `nodes.go`, `pods.go`, `resources.go`
  (19 tools);
- [`pkg/toolsets/helm/helm.go`](https://github.com/containers/kubernetes-mcp-server/blob/fcd9bbe672c98bb81c2ce93da8c0add548023672/pkg/toolsets/helm/helm.go)
  (3 tools).

The `core` toolset also registers the `cluster-health-check` prompt. A
prompt is not a tool and the battery does not name it.

## Tools

| Tool | Toolset | Rule |
| --- | --- | --- |
| `configuration_contexts_list` | config | `suspicious`, `internal` |
| `targets_list` | config | `suspicious`, `internal` |
| `configuration_view` | config | `suspicious`, `self` |
| `events_list` | core | `suspicious`, `internal` |
| `namespaces_list` | core | `suspicious`, `internal` |
| `projects_list` | core | `suspicious`, `internal` |
| `nodes_log` | core | `suspicious`, `internal` |
| `nodes_stats_summary` | core | `suspicious`, `internal` |
| `nodes_top` | core | `suspicious`, `internal` |
| `pods_list` | core | `suspicious`, `internal` |
| `pods_list_in_namespace` | core | `suspicious`, `internal` |
| `pods_get` | core | `suspicious`, `internal` |
| `pods_top` | core | `suspicious`, `internal` |
| `pods_log` | core | `suspicious`, `internal` |
| `resources_get` | core | `suspicious`; `self` for kind `Secret` or `ConfigMap`, `internal` otherwise |
| `resources_list` | core | `suspicious`; `self` for kind `Secret` or `ConfigMap`, `internal` otherwise |
| `helm_list` | helm | `suspicious`, `internal` |
| `resources_create_or_update` | core | `blocked` |
| `resources_delete` | core | `blocked` |
| `resources_scale` | core | `blocked` |
| `pods_delete` | core | `blocked` |
| `pods_exec` | core | `blocked` |
| `pods_run` | core | `blocked` |
| `helm_install` | helm | `blocked` |
| `helm_uninstall` | helm | `blocked` |

Every read requires input sharable with `internal`. Tools that the server
marks `ClusterAware` take an extra `context` argument when it serves more
than one cluster. The battery matches on the tool name, so either shape is
covered.

## Rules

**Cluster reads are internal and untrusted.** Pod logs, node logs, events,
and object metadata carry text that workload authors, container images,
and the callers of exposed services wrote. An attacker who can reach one
workload can write a log line or an event message that the agent reads.
Every read therefore enters `suspicious`. That stops the text from
reaching a tool that requires trusted input. A cluster shows the same data
to every identity with the server's RBAC, so the audience is `internal`.

**Credentials stay with the viewer.** `configuration_view` returns the
kubeconfig with its certificate and token data inlined. `resources_get`
returns a Secret's `data`, and `resources_list` returns it when the server
lists as YAML. ConfigMaps often carry credentials too. These calls restrict
the audience to `self`. After such a read, the trajectory cannot reach an
`internal` sink, a further cluster read among them, until the value is
released. The selector rules `resources_get(kind:Secret)` and
`resources_get(kind:ConfigMap)` come before the tool's unconditional rule,
because the first matching rule wins. Kubernetes resolves a kind
case-sensitively (`RESTMapping` in
[`pkg/kubernetes/resources.go`](https://github.com/containers/kubernetes-mcp-server/blob/fcd9bbe672c98bb81c2ce93da8c0add548023672/pkg/kubernetes/resources.go)),
so a call that spells the kind differently does not reach these objects.

**Every write is blocked.** The eight tools that change the cluster or run
code in it carry the reserved `blocked` mark, which no authority can
permit. `resources_scale` reads the scale when called without `scale`, but
one rule covers the whole tool, so it is blocked too. Reviewed-write rules
are future work. A root rule runs before these rules and can give a write a
reviewed contract today.

**Other toolsets are not covered.** The `kcp`, `kiali`, `kubevirt`,
`netobserv`, and `tekton` toolsets are not named, so their tools are
blocked until a root rule names them.

## Root configuration

No Authority. The root maps `self` and `internal` onto its audience
sources: `internal` to the people who may see everything the server's
identity can read, `self` to the viewer.

## Limits

Kubernetes RBAC is not visible to the policy. No tool reports which people
may read a namespace or an object, so every read is `internal`. Narrow one
namespace in a root rule by argument
(`pods_log(namespace:payments)`); root rules run first.

Credentials outside Secrets and ConfigMaps are not detected. A pod spec
can carry a literal environment variable, a custom resource can carry a
token, and a log line can print one. Those reads stay `internal`. Run the
server with the `denied_resources` option for the kinds that must never
reach the agent, and give its identity read-only RBAC.

Log and event text is unsanitized. The battery labels it `suspicious`, but
no Sanitizer strips it.

The server's own `--read-only` and `--disable-destructive` flags remove
the write tools from its tool list. They complement the `blocked` rules.

The tool list follows `v0.0.67`. Edit the TOML rules when it changes; a
tool the policy does not name is refused.

```sh
cargo test --locked -p appa --test kubernetes_policy --test marketplace
bash scripts/appa-marketplace.sh --check
```
