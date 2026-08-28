---
title: kagent integration
category: Integration
order: 8
description: Proposal for enforcing OpenAPPA policy in the kagent Go runtime.
---

:::proposal
name: kagent integration
date: 2026-08-28
author: Mark Novikov

This proposal adds an OpenAPPA execution profile to the kagent Go runtime.

The profile authorizes the exact arguments that kagent passes to a tool. It also controls the result before any consumer receives it.

The profile refuses a path when kagent cannot observe both boundaries.

This proposal uses [kagent commit `9e246fd37`](https://github.com/kagent-dev/kagent/commit/9e246fd3797457b18fc277680be1629a0f57fce0) as its source baseline.

OpenAPPA policy semantics remain in [How it works](/how-it-works) and [Policy contracts](/contracts). This proposal covers only the kagent integration.

## Gate ordinary tool calls twice

kagent uses Google ADK to run model, tool, and child-agent steps. An ordinary ADK function tool uses two existing boundaries:

1. `BeforeToolCallbacks` send `HookEvent::ToolCall` before tool dispatch.
2. `AfterToolCallbacks` send one terminal `HookEvent::ToolResult` after execution or error handling.

:::kagent-enforcement:::

The Go extension sends both events to the local `appa-runtime` process. It applies the returned decision before kagent continues.

OpenAPPA can block dispatch, keep the returned result, replace it, or withhold it.

## Install with Helm

The integration release publishes one OCI Helm chart. It pins both images and includes an OpenAPPA `Harness` template that is disabled by default.

Create the policy `ConfigMap`:

```sh
kubectl create namespace kagent --dry-run=client -o yaml | kubectl apply -f -
kubectl -n kagent create configmap customer-support-policy \
  --from-file=appa.toml=./appa.toml \
  --dry-run=client -o yaml | kubectl apply -f -
```

Apply the generated CRDs, then install or upgrade kagent:

```yaml
# openappa-values.yaml
openappa:
  policyRef: customer-support-policy
  harness:
    enabled: false
  allowedAgentTemplates:
    matchLabels:
      openappa: enabled
substrate:
  workerPoolRef: default
  snapshotLocation: s3://kagent-snapshots/openappa
```

```sh
helm show crds oci://ghcr.io/archestra-ai/charts/kagent-openappa \
  --version <release-version> | kubectl apply -f -

helm upgrade --install kagent \
  oci://ghcr.io/archestra-ai/charts/kagent-openappa \
  --version <release-version> \
  --namespace kagent \
  --values openappa-values.yaml \
  --atomic --wait --timeout 5m
```

Wait for the patched controller and its service endpoint:

```sh
kubectl -n kagent rollout status deployment/kagent-controller --timeout=5m
kubectl -n kagent wait endpoints/kagent-controller \
  --for='jsonpath={.subsets[0].addresses[0].ip}' --timeout=5m
```

Enable the chart-managed `Harness`, label the selected `AgentTemplate`, and wait for preparation:

```sh
helm upgrade kagent oci://ghcr.io/archestra-ai/charts/kagent-openappa \
  --version <release-version> \
  --namespace kagent \
  --values openappa-values.yaml \
  --set openappa.harness.enabled=true \
  --atomic --wait --timeout 5m

kubectl -n kagent label agenttemplate customer-support-agent openappa=enabled
kubectl -n kagent wait agenttemplate/customer-support-agent \
  --for='jsonpath={.status.harnesses[?(@.harness=="kagent-openappa")].conditions[?(@.type=="Ready")].status}=True' \
  --timeout=5m
```

The integration adds a `kagentctl` command for creating the protected instance:

```sh
kagentctl agent-instance create customer-support \
  --namespace kagent \
  --template customer-support-agent \
  --harness kagent-openappa
```

## Run OpenAPPA inside the Actor

The Actor image runs `kagent-go-adk` and `appa-runtime` as separate processes in one container.

The Actor becomes ready only after `appa-runtime` passes its health check.

:::kagent-profile:::

The Go extension implements existing ADK callback and plugin interfaces inside `kagent-go-adk`.

Each callback sends kagent `JSON` to `http://127.0.0.1:8787/hook`. `appa-adapter-kagent` translates the request into `HookEvent` and `HookDecision` values.

kagent declares `/data` as durable storage in the `ActorTemplate`. Substrate mounts it when it creates the `Actor`.

OpenAPPA stores its database at `/data/openappa/appa.db`.

## Map every execution path

Most paths use existing Google ADK callbacks. The table marks proposed kagent callbacks explicitly.

| Execution path | Dispatch boundary | Result boundary |
|---|---|---|
| Normal function, MCP, skill, or model-called memory tool | `BeforeToolCallbacks` → `HookEvent::ToolCall` | `OnToolErrorCallbacks`, then `AfterToolCallbacks` → `HookEvent::ToolResult` |
| Local ADK `chat` transfer | `BeforeToolCallbacks` on `transfer_to_agent` → `HookEvent::ToolCall` | No bounded child result. `BeforeAgentCallbacks` keep the same trajectory |
| Local ADK `single_turn` | `BeforeToolCallbacks` → `ToolCall`. `BeforeAgentCallbacks` → `ChildStart` | `AfterToolCallbacks` → `SpawnResult` |
| Local ADK `task` | `AfterModelCallbacks` → `ToolCall`. `BeforeAgentCallbacks` → `ChildStart` | `plugin.OnEventCallback` → `SpawnResult` |
| Remote A2A agent | `BeforeToolCallbacks` → `ToolCall` and `ChildStart` | `AfterToolCallbacks` → `SpawnResult` |
| Automatic `preload_memory` | Context-bound `memory.Service` decorator before `Search` | The same decorator before model delivery |
| MCP App internal tool or resource | Proposed owning-Actor gate → `ToolCall` | The same proposed gate → `ToolResult` |
| Registered long-running work | Proposed `BeforeBackgroundStart` → `ToolCall` and `ChildStart` | Proposed `OnBackgroundResult` → `SpawnResult` |

`BeforeModelCallbacks` confirm that each model-bound result already passed admission. `plugin.OnUserMessageCallback` rejects forged task responses.

Trusted deployment configuration must bind the authenticated remote endpoint to an isolated OpenAPPA `Harness`.

A direct message or `completed` task can return content. `input_required` and `auth_required` pause the remote call.

`failed`, `canceled`, and `rejected` produce a failed `SpawnResult` without content. `submitted` and `working` continue waiting or fail without a parent result.

## Check first-release limits

The first implementation refuses these paths:

- Streaming tool chunks and provider-native tools.
- Unregistered background work, asynchronous MCP jobs, and notification-only results.
- Remote or BYO agents without a compatible OpenAPPA adapter.
- Python ADK, OpenAI Agents, LangGraph, and CrewAI without framework adapters.

The implementation plan contains the complete supported and refused path catalog.

## Fail closed

kagent prepares no revision until coverage validation passes. The `Actor` accepts no work until `appa-runtime` reports ready.

A runtime failure before dispatch blocks the call. A runtime failure after execution withholds the result.

The profile rejects kagent `RequireApproval` configuration.

The runtime currently registers `MakeApprovalCallback` first in `BeforeToolCallbacks`, so it can return before the OpenAPPA callback.

The profile also rejects any added callback or plugin that can bypass an OpenAPPA boundary.

Cancellation reports `ToolOutcome::Indeterminate` before the profile releases the trajectory permit.

After Actor recovery, the profile sends `TurnEnd` before a new call to close an unknown outstanding dispatch.

One trajectory runs one tool call at a time. Separate child trajectories can run concurrently.

## Adopt side by side

An existing `AgentInstance` cannot switch to another `Harness` or prepared revision. Existing clusters adopt OpenAPPA through a side-by-side replacement.

| Phase | Operator action | Routing state |
|---|---|---|
| Prepare | Apply the policy `ConfigMap` and values with the `Harness` disabled | Existing instances continue serving |
| Upgrade | Apply chart CRDs, upgrade the control plane, and wait for its service endpoint | Existing instances continue on current revisions |
| Enable | Enable the chart-managed `Harness`, label the template, and wait for its ready revision | Existing instances continue serving |
| Canary | Create a protected `AgentInstance` and run internal checks | Production traffic remains on the old instance |
| Cut over | Route new root A2A tasks to protected instance | Pin old task and context IDs to old instance |
| Drain | Wait for terminal work or apply the timeout and cancellation policy | No new work reaches the old instance |
| Retain | Suspend old instance, export records, and verify `/data` snapshot | Keep both for at least 30 days |

The protected instance starts a new OpenAPPA database and new root trajectories. The first release does not import an old unprotected transcript.

When the application uses an external ADK session store, migration starts a new protected session ID.

Migration excludes the old model context.

External records re-enter through protected tools. Their tool contracts assign the required policy Labels.

Rollback routes new root tasks to the unchanged old instance. Existing protected task and context IDs stay protected until terminal or canceled.

After rollback drain, the operator suspends the protected instance and exports its task and event records.

The operator verifies that the Substrate snapshot contains `/data`, then retains the export and snapshot for at least 30 days.

Deletion requires a separate operator decision after the retention period. Rollback never merges or rewrites OpenAPPA trajectory logs.

## Implementation plan

The [kagent implementation plan](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) contains exact APIs, source ownership, and callback ordering.

It also contains coverage tests and the upstream contribution sequence.
:::
