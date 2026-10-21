---
title: Kubernetes battery
category: Batteries
order: 6.77
description: Rules for the Kubernetes MCP server's config, core, and helm toolsets, with internal untrusted reads, viewer-only credentials, and every write blocked.
sidebar: false
breadcrumb: Kubernetes
---

The Kubernetes battery covers 25 tools of [`containers/kubernetes-mcp-server`](https://github.com/containers/kubernetes-mcp-server) `v0.0.67`: the default `config` and `core` toolsets and the optional `helm` toolset. Seventeen tools read; eight change the cluster or run code in it.

The battery needs no Authority. Your root config maps `self` and `internal` onto its audience sources.

[View the battery source](https://github.com/archestra-ai/OpenAPPA/tree/main/marketplace/batteries/kubernetes).

## Tool behavior

- Pod logs, node logs, events, and object metadata carry text that workloads and their callers wrote. Every read returns untrusted `internal` data, and a read's input must be sharable with `internal`.
- `configuration_view` returns the kubeconfig with its credentials inlined. `resources_get` and `resources_list` for kind `Secret` or `ConfigMap` return data that often holds credentials. These reads restrict the audience to `self`.
- The eight write and execution tools, `resources_create_or_update`, `resources_delete`, `resources_scale`, `pods_delete`, `pods_exec`, `pods_run`, `helm_install`, and `helm_uninstall`, carry the `blocked` mark. No authority can permit them. A root rule can give a write a reviewed contract.
- The `kcp`, `kiali`, `kubevirt`, `netobserv`, and `tekton` toolsets are not named, so their tools are refused.

## Limit

Kubernetes RBAC is not visible to the policy, so every read defaults to `internal`. Map it in your root `appa.toml` to the people who may see everything the server's identity can read.

Credentials outside Secrets and ConfigMaps, such as a literal environment variable in a pod spec or a token printed to a log, stay `internal`. Use the server's `denied_resources` option and read-only RBAC for the kinds that must never reach the agent.
