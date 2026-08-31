# kagent integration implementation plan

Status: proposed

Source baselines:

- kagent commit `9e246fd3797457b18fc277680be1629a0f57fce0`
- Google ADK Go tag `v2.2.0`
- OpenAPPA runtime contract in `appa-runtime-api/src/lib.rs`

The reader-facing proposal is at [openappa.com/kagent](https://www.openappa.com/kagent).

## Goal

Add an optional OpenAPPA profile to the kagent Go runtime without changing behavior for existing Harnesses.

The code MUST preserve four invariants:

1. OpenAPPA authorizes the final JSON argument value passed to `tool.Run`.
2. No consumer receives a result before OpenAPPA admission.
3. Every root, child, call, and terminal outcome has stable identity.
4. kagent refuses any enabled path that lacks a dispatch or result boundary.

## Deliverables

| Owner | Change |
|---|---|
| `go/api/v1alpha3` | Add optional OpenAPPA Harness configuration and local child collaboration mode |
| `go/core/v2/translator/kagent` | Resolve policy, compile profile data, and validate path coverage |
| `go/core/v2/substrate` | Include policy and egress digests in the prepared revision and `ActorTemplate` |
| `go/adk/pkg/agent` | Register OpenAPPA callbacks on the root and every local child |
| `go/adk/pkg/config` | Materialize the policy bundle from compiled Actor configuration |
| `go/adk/pkg/runner` | Register the task plugin and memory decorator |
| `go/adk/pkg/tools` | Wrap final tool execution and preserve remote A2A context and `TaskState` |
| `go/adk/cmd/openappa-supervisor` | Start and monitor `appa-runtime` and `kagent-go-adk` |
| `go/core/internal/grpcserver` | Route protected MCP App internal calls to the owning Actor |
| `appa-adapter-kagent` | Translate kagent JSON to `HookEvent` and `HookDecision` |
| Helm chart and `kagentctl` | Install the profile and create protected `AgentInstance` resources |

## Package the profile

Extend `KagentHarness` with an optional `OpenAPPA` block containing a same-namespace policy `ConfigMap` reference.

The compiler MUST:

1. Resolve and validate the policy bundle.
2. Include all policy bytes and external endpoint destinations in the revision digest.
3. Require an explicit external LLM URL when policy can invoke one.
4. Add the validated files and digest to `AgentConfig.OpenAPPA` in `KAGENT_CONFIG_JSON`.
5. Set `APPA_CONFIG`, `APPA_DB`, and `APPA_RUNTIME_URL` for the Actor image.

`go/adk/pkg/config` decodes `AgentConfig.OpenAPPA`, verifies the digest, and writes files atomically under `/data/openappa/policy/<digest>`.

The Actor image contains `kagent-openappa-supervisor`, `kagent-go-adk`, and `appa-runtime`.

The supervisor starts `appa-runtime`, waits for `/health`, then starts `kagent-go-adk`. It forwards signals and exits when either child exits.

`/readyz` fails whenever `appa-runtime` is unavailable. Runtime state remains at `/data/openappa/appa.db`.

The OCI chart at `ghcr.io/archestra-ai/charts/kagent-openappa` pins the control-plane and Actor image digests.

Chart values map to these Harness fields:

```yaml
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

Installation is two-phase:

1. Apply CRDs and upgrade the control plane with `openappa.harness.enabled=false`.
2. Wait for the controller Deployment and service endpoint.
3. Upgrade again with `openappa.harness.enabled=true`.
4. Label each admitted `AgentTemplate` and wait for its Harness-specific `Ready=True` condition.

`kagentctl agent-instance create --template <name> --harness kagent-openappa` maps directly to `AgentInstanceService.CreateAgentInstance`.

## Add the Go ADK extension

The Go extension owns transport and correlation. It contains no policy logic.

Keep current exported builder signatures. Add extension-aware variants that accept one optional `OpenAPPAExtensions` value and pass it recursively to local children.

Register the OpenAPPA plugin first in `PluginConfig.Plugins`. Reject kagent `RequireApproval` and any other callback or plugin that can short-circuit `BeforeTool`.

Wrap every executable tool with `OpenAPPATool`. Its `Run` method compares the final argument bytes with the authorized snapshot immediately before delegating to the underlying tool.

A mismatch closes the open dispatch as indeterminate and refuses underlying execution.

| ADK point | OpenAPPA action |
|---|---|
| `BeforeToolCallbacks` | Snapshot final arguments and send `ToolCall` |
| `OnToolErrorCallbacks` | Record failure state and replace unsafe error text |
| `AfterToolCallbacks` | Send one terminal `ToolResult` for each released call |
| `BeforeModelCallbacks` | Confirm model-bound results already passed admission |
| `BeforeAgentCallbacks` | Select or bind the local child trajectory |
| `AfterModelCallbacks` | Gate deferred ADK task dispatch |
| `plugin.OnEventCallback` | Gate the synthesized task return before persistence |
| `plugin.OnUserMessageCallback` | Reject forged task responses |

The OpenAPPA `OnToolError` plugin callback runs before kagent error logging. It stores failure state and returns `{"error":"tool execution failed"}`.

The first `AfterTool` callback sends the terminal outcome. `ReplaceOutput` or `Block` returns admitted replacement content and stops later callback observation.

The pending-call store uses the function-call ID as its key and synchronizes access.

| State | Meaning | `AfterTool` action |
|---|---|---|
| `Released` | OpenAPPA returned `AllowCall` | Send one terminal outcome |
| `Denied` | OpenAPPA denied dispatch | Preserve the denial and send no outcome |
| `SnapshotFailed` | Arguments could not serialize | Refuse and send no outcome |
| `DeferredTask` | ADK emitted task-mode placeholder callbacks | Let the task plugin own both boundaries |
| Missing | Another callback bypassed OpenAPPA | Replace all output with a refusal |

## Bind the executed call

The OpenAPPA `BeforeTool` callback serializes the ADK argument map and stores the authorized bytes.

Send those bytes as `RawValue`. Do not require model-provider transport bytes.

Immediately before execution, `OpenAPPATool.Run` serializes the final map again and requires byte equality with the authorized snapshot.

On equality, the wrapper recursively copies the JSON map and passes only that unshared copy to the underlying tool.

Serialization failure or mismatch refuses the call and closes the dispatch as indeterminate.

Keep the serialized call in memory until `AfterTool`. Send the same bytes with `ToolResult` because the runtime identifies outcomes through canonical call arguments.

Allow only one open dispatch per trajectory. Hold a trajectory permit from pre-dispatch until terminal outcome admission.

Cancellation reports `ToolOutcome::Indeterminate` before permit release. Actor recovery sends `TurnEnd` before the next call.

## Cover each execution path

| Path | Implementation |
|---|---|
| Normal function, MCP, skill, or memory tool | Existing tool callbacks |
| Local `chat` | Gate `transfer_to_agent` and keep the parent trajectory |
| Local `single_turn` | `BeforeTool` prepares spawn, `BeforeAgent` sends `ChildStart`, `AfterTool` sends `SpawnResult` |
| Local `task` | `AfterModel` prepares spawn, `BeforeAgent` sends `ChildStart`, `OnEvent` sends `SpawnResult` |
| Remote A2A | Allocate isolated context before dispatch, send `ChildStart`, preserve terminal `TaskState`, send `SpawnResult` |
| Automatic `preload_memory` | Wrap `memory.Service` and bind the acting trajectory through context |
| MCP App internal call | Route the request to the owning Actor with a short-lived trajectory capability |
| Registered long-running work | Add optional `BeforeBackgroundStart` and `OnBackgroundResult` parameters to kagent launch and resume functions |

ADK task placeholder callbacks MUST emit no OpenAPPA event. The task plugin is the only owner of task dispatch and return.

Remote A2A rules:

- Require `isolateSessions=true` and bind the authenticated endpoint to an OpenAPPA-capable Harness.
- Allow content from a direct message or `completed` task only.
- Pause on `input_required` or `auth_required`.
- Report `failed`, `canceled`, and `rejected` without content.
- Continue waiting or fail on `submitted`, `working`, or unknown state.

The first implementation refuses streaming chunks, provider-native tools, unregistered background work, notification-only results, and uninstrumented non-Go frameworks.

## Keep result publication ordered

OpenAPPA admission MUST finish before kagent constructs or publishes result content.

Apply this barrier before model input, parent return, A2A streams, memory, MCP App UI, content logs, and background storage.

For MCP App internal calls, extend tool and resource requests with `agent_instance_id`, `request_id`, and an opaque capability.

The Actor mints a random 256-bit capability only after admission of the model-called App result.

Store only its hash. Bind it to root trajectory, actor, caller identity, MCP server, allowed tool or resource scope, and expiry.

The controller forwards the request to the owning Actor and performs no direct MCP execution.

The Actor rejects missing, expired, replayed, used-request, caller, scope, server, actor, or trajectory mismatches before `ToolCall`.

After the Actor validates the capability, it runs both OpenAPPA gates and records the `request_id` against replay.

## Preserve remedy control

Register the local `appa-runtime` MCP endpoint on the root and every protected local child.

`execute_remedy_plan` MUST bind its offer to the acting trajectory. The control tool never executes a substituted call itself.

The next model-proposed call crosses normal dispatch enforcement with the returned tool name and JSON arguments.

## Existing-cluster adoption

Do not mutate an existing `AgentInstance` into the OpenAPPA profile.

1. Run the two-phase Helm upgrade.
2. Create a new protected instance from the existing `AgentTemplate`.
3. Route only new root tasks and contexts to the new instance ID.
4. Keep existing task and context IDs pinned to the old instance until terminal or canceled.
5. Suspend the old instance.

The protected instance starts a new ADK session, OpenAPPA database, and root trajectory family. Do not import the old model transcript.

Rollback resumes the old instance and routes new roots back to it.

Protected task and context IDs remain on the protected instance until terminal or canceled. Rollback never merges OpenAPPA logs.

## Upstream sequence

| PR | Generic kagent change |
|---|---|
| 1 | Add extension-aware Go ADK builders |
| 2 | Add local collaboration mode to `AgentToolBinding` |
| 3 | Add task dispatch and return plugin seams |
| 4 | Preserve remote A2A `TaskState` and preallocate context IDs |
| 5 | Add result-publication barriers and dynamic path validation |
| 6 | Add background-work lifecycle callbacks |

The OpenAPPA Harness profile, policy compiler, Go extension, and Rust adapter remain integration-specific.

## Verification

| Area | Required coverage |
|---|---|
| Compatibility | Existing Harnesses, builders, CRDs, callbacks, approval, and concurrency remain unchanged without the profile |
| Tool calls | Allow, deny, argument snapshot, mutation attempt, serialization error, failure, cancellation, and replacement |
| Callback safety | Short-circuit rejection, native approval, error logger order, missing state, and post-authorization mutation |
| Concurrency | Same-trajectory serialization, child parallelism, cancellation, panic, and permit cleanup |
| Local children | `chat`, `single_turn`, `task`, forged response, pause, resume, cancellation, and restart |
| Remote A2A | Isolation, endpoint binding, direct message, every task state, replacement, and mismatch |
| Memory and MCP Apps | Preload context, result barrier, capability expiry, resource read, and UI publication |
| MCP App capability | Caller, actor, trajectory, server, scope, expiry, request replay, and capability mismatch |
| Policy and discovery | Policy digest, egress allowlist, explicit LLM URL, MCP discovery failure, and dynamic tool validation |
| Remedies | Offer trajectory binding, stale offer, authorization, substitution, returned value, decline, and no answer |
| Migration | CRD order, two-phase Helm upgrade, ready revision, task affinity, fresh session, replacement, and rollback |
| Runtime | Health, process exit, database persistence, policy revision, and recovery |

The implementation is complete when every enabled path reaches its required dispatch and result boundary.
