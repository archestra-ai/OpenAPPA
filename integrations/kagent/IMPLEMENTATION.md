# kagent integration implementation plan

Status: proposed

Source baselines:

- kagent commit `9e246fd3797457b18fc277680be1629a0f57fce0`
- Google ADK Go tag `v2.2.0`
- OpenAPPA runtime API in `appa-runtime-api/src/lib.rs`

The reader-facing proposal is available at [openappa.com/kagent](https://www.openappa.com/kagent).

## Invariants

The implementation MUST preserve these properties:

1. OpenAPPA authorizes the final JSON-semantic argument value that kagent passes to `tool.Run`, not provider wire bytes.
2. kagent publishes no returned content before OpenAPPA admission.
3. Each root, child, call, and pending outcome has a stable identity.
4. Uncovered execution paths fail before they handle protected content.
5. Existing Harnesses keep current behavior when OpenAPPA is absent.

## Change map

| Owner | Change |
|---|---|
| `go/api/v1alpha3` | Add the optional OpenAPPA Harness profile and local collaboration mode |
| `go/core/v2/translator/kagent` | Resolve policy, compile profile data, and validate supported paths |
| `go/core/v2/substrate` | Include compiled OpenAPPA data in the immutable revision |
| `go/adk/pkg/config` | Materialize the policy bundle and expose process configuration |
| `go/adk/pkg/agent` | Inject callbacks and snapshot final ADK arguments |
| `go/adk/pkg/runner` | Add the OpenAPPA plugin and memory-service decorator |
| `go/adk/pkg/tools` | Preserve remote A2A task state and child identity |
| OpenAPPA | Add `appa-adapter-kagent` and the `kagent` runtime adapter option |

## Release artifacts

The integration release publishes these artifacts:

| Artifact | Producer | Consumer |
|---|---|---|
| Generated CRD and Helm bundle | Integration release CI from the kagent fork | Cluster operator upgrade |
| Patched kagent control-plane image | Integration release CI from the kagent fork | Cluster operator deployment |
| OpenAPPA Actor runtime image | Integration release CI | OpenAPPA `Harness` workload |
| OpenAPPA `Harness` manifest | Integration release | Kubernetes API |
| Example policy `ConfigMap` | OpenAPPA repository | Policy author and cluster operator |

The Actor image contains the patched kagent Go runtime and the `appa-runtime` binary from the same reviewed release.

The `Harness` manifest MUST pin the Actor image by SHA-256 digest.

Release notes MUST record the CRD bundle version, both image digests, and source commits.

The cluster operator installs the patched control plane before applying the `Harness`. Existing unpatched control planes reject the new `openappa` field.

## Harness profile

Extend the existing empty `KagentHarness` type:

```go
type KagentHarness struct {
    OpenAPPA *OpenAPPAHarnessProfile `json:"openappa,omitempty"`
}

type OpenAPPAHarnessProfile struct {
    PolicyRef corev1.LocalObjectReference `json:"policyRef"`
}
```

The Harness compiler performs these operations:

1. Resolve the same-namespace policy ConfigMap.
2. Require one `appa.toml` key and reject unsafe relative file names.
3. Validate the OpenAPPA configuration during revision preparation.
4. Add ConfigMap identity and content hash to revision provenance.
5. Add the policy bundle and digest to the compiled `adk.AgentConfig`.

The revision digest MUST cover the complete policy bundle. A policy change creates a new prepared revision.

Parse every configured external authority, sanitizer, cast, resolver, membership, and LLM endpoint after policy validation.

Add each HTTPS destination to `Revision.EgressDestinations`. The destination set and policy bytes MUST affect the revision digest.

The profile requires an explicit `[externals.llm].url` when policy can invoke an external LLM. It does not use an implicit provider endpoint.

## Local process model

kagent generates a one-container `ActorTemplate` with `DurableDirs: [{Path: "/data"}]`.

Substrate provisions and mounts that durable directory when it creates the `Actor`. The integration does not create a Kubernetes PVC directly.

The `Harness` worker pool and snapshot policy control storage placement and Actor checkpoint behavior.

The OpenAPPA profile uses two runtime processes in that container.

```text
/usr/local/bin/kagent-openappa-supervisor
  |-- /usr/local/bin/appa-runtime --adapter kagent
  `-- /usr/local/bin/kagent-go-adk

/data/openappa/
  |-- policy/<revision>/appa.toml
  `-- appa.db
```

Extend compiled `adk.AgentConfig` with an internal OpenAPPA bundle:

```go
type OpenAPPACompiledConfig struct {
    Files  map[string]string `json:"files"`
    Digest string            `json:"digest"`
}
```

Add `go/adk/cmd/openappa-supervisor/main.go`. Build it as the Actor image entrypoint and PID 1.

The Actor image build copies three reviewed binaries into `/usr/local/bin`: the supervisor, patched kagent Go runtime, and `appa-runtime`.

The supervisor uses `os/exec` to start both children in one process group. It forwards `SIGTERM` and `SIGINT`, reaps children, and enforces shutdown timeouts.

The runtime materializer writes each file atomically under `/data/openappa/policy/<digest>`. It verifies the digest before startup.

The supervisor starts `appa-runtime` first and waits for `/health`. It starts `kagent-go-adk` only after runtime health succeeds.

The Go `/readyz` endpoint MUST fail when the local runtime is unavailable.

The supervisor exits with failure when either child process exits. Substrate then applies its configured `Actor` lifecycle policy.

Use these process settings:

```text
APPA_RUNTIME_URL=http://127.0.0.1:8787
APPA_CONFIG=/data/openappa/policy/<digest>/appa.toml
APPA_DB=/data/openappa/appa.db
```

## OpenAPPA adapter

Add a pure `appa-adapter-kagent` crate beside `appa-adapter-claude-code`.

The crate depends only on `appa-runtime-api`. It parses kagent wire JSON into `HookEvent` and renders `HookDecision` into kagent wire JSON.

Add `Kagent` to the runtime `Adapter` enum and select the new codec for `--adapter kagent`.

The adapter MUST hold no state, call no policy API, and perform no external I/O.

## Go ADK extension

The Go extension implements existing ADK callbacks and one runner plugin. It owns transport and correlation but contains no policy logic.

```go
type OpenAPPAExtensions struct {
    Client            *HookClient
    AgentCallbacks    AgentCallbackSet
    Plugin            *plugin.Plugin
    MemoryService     memory.Service
    SupportedToolsets ToolsetCatalog
}
```

Final names follow kagent conventions. These responsibilities do not change.

### Builder compatibility

Keep current exported builder signatures:

```text
CreateRunnerConfig(...)
  -> createRunnerConfig(..., extensions = nil)

CreateGoogleADKAgent(...)
  -> createGoogleADKAgent(..., extensions = nil)
```

Add extension-aware entry points for the OpenAPPA Harness:

```text
CreateRunnerConfigWithExtensions(..., extensions)
  -> createRunnerConfig(..., extensions)
  -> CreateGoogleADKAgentWithExtensions(..., extensions)
  -> createGoogleADKAgent(..., extensions)
  -> recurse with the same extensions
```

### Callback order

```text
BeforeTool   OpenAPPA argument snapshot and decision -> tool
OnToolError capture failure -> safe error -> mark outcome pending
AfterTool    if released, report one success or failure -> publication
BeforeModel OpenAPPA admission assertion -> model
AfterModel  OpenAPPA task dispatch gate -> native task dispatch
OnEvent     OpenAPPA task return gate -> persistence -> parent model
```

The profile rejects kagent `RequireApproval` and any non-OpenAPPA callback or plugin that can short-circuit `BeforeTool`.

Use an OpenAPPA authority and remedy plan when a policy requires human approval.

The OpenAPPA callback MUST finish before kagent constructs or publishes tool-result content.

`OnToolError` MUST NOT send `ToolResult`. It stores failure state in the invocation context.

Track one gate state by function-call ID:

| State | Meaning | `AfterTool` behavior |
|---|---|---|
| `Released` | OpenAPPA returned `AllowCall` | Report exactly one terminal `ToolResult` |
| `Denied` | OpenAPPA denied the call | Send no `ToolResult`. Preserve the rendered denial |
| `SnapshotFailed` | Final arguments could not serialize | Send no `ToolResult`. Preserve the fail-closed response |
| `DeferredTask` | ADK emitted the task-mode no-op callbacks | Send no `ToolResult`. The task plugin owns both gates |
| Missing | Another callback bypassed OpenAPPA | Replace all output with a fail-closed refusal |

For `Released`, `AfterTool` reads failure state and sends exactly one outcome. Tests MUST assert one outcome for each release.

## Authoritative argument snapshot

ADK passes one `map[string]any` value through `BeforeToolCallbacks` and then into `tool.Run`.

Register the OpenAPPA callback last in the `BeforeTool` chain. No later callback can change the arguments after authorization.

Callbacks MUST NOT retain or mutate `args` after they return. The pending-call store MUST synchronize access across concurrent ADK tool workers.

Coverage validation MUST reject every other runner or agent `BeforeToolCallback` under this profile.

Serialize that final ADK map once:

```go
type PendingToolCall struct {
    ID        string
    Tool      string
    Arguments json.RawMessage
}
```

Send those bytes to OpenAPPA as `RawValue`. Here, raw means unparsed at the adapter boundary, not original provider wire bytes.

The tool receives the same unchanged ADK map after `AllowCall`. A serialization error refuses the call.

If kagent changed that map after `AllowCall`, the tool would execute arguments that OpenAPPA never authorized.

Duplicate object keys no longer exist at this boundary, and the tool cannot receive them. OpenAPPA validates the JSON value that the tool receives.

Keep the serialized snapshot in memory until `AfterTool` reports the terminal outcome. kagent does not write this record to disk.

`HookEvent::ToolResult` has no independent call ID. It carries the proposed call, so kagent needs the snapshot to identify which released call produced the outcome.

Send the stored bytes with `ToolResult`. `appa-runtime` compares their canonical JSON arguments with the released call.

Test final callback ordering, later mutation attempts, unsupported JSON values, numeric values, serialization failure, and outcome correlation.

`appa-runtime` persists the released call. On Actor recovery, send `TurnEnd` before any new call to close an unknown outcome.

## Remedy control tool

Register the local `appa-runtime` MCP endpoint with the root agent and every protected local child.

The reserved `execute_remedy_plan` tool follows this flow:

```text
model proposes execute_remedy_plan(offer_id)
  -> BeforeTool identifies the exact reserved tool
  -> appa-runtime vouches the acting trajectory
  -> PassControl permits the MCP call
  -> MCP endpoint consumes the vouch and judges the live offer
```

An offer from another trajectory MUST fail. An offer ID alone grants no authority.

`Authorized` and `Substituted` outcomes return the exact tool name and arguments to propose next.

The control tool MUST NOT execute that call itself. The next model-proposed call crosses normal enforcement with the returned JSON-semantic arguments.

`Returned`, `Declined`, and `NoAnswer` remain normal control-tool results.

## Dispatch serialization

The current OpenAPPA host contract permits one open dispatch per trajectory.

Wait for a trajectory permit in the pre-dispatch callback. Do not send a second `ToolCall` while the first dispatch remains open.

Release the permit after `AfterTool` or terminal child return admission.

Cancellation MUST report an indeterminate outcome before permit release. Separate child trajectories can run in parallel.

## Local collaboration modes

Add an optional mode to `AgentToolBinding`:

```go
type AgentToolBinding struct {
    Name        string                 `json:"name"`
    Description string                 `json:"description"`
    TemplateRef AgentTemplateLocalReference `json:"templateRef"`
    Isolation   AgentToolIsolation     `json:"isolation,omitempty"`
    Mode        AgentCollaborationMode `json:"mode,omitempty"`
}
```

Supported values are `chat`, `single_turn`, and `task`. An omitted value preserves the current `chat` behavior.

The compiler carries the mode into the Go ADK child configuration and validates the required OpenAPPA capability.

`BeforeAgentCallbacks` select the trajectory context for the activated local agent. `AfterAgentCallbacks` clear that activation context.

These callbacks do not report a child return. `AfterTool` or the task `OnEvent` gate owns the parent-facing result.

The active trajectory context also selects the correct memory-service decorator.

### Chat

The transfer tool runs through normal tool callbacks. The selected local agent continues on the parent trajectory and Label.

There is no separate child-return boundary. Every later tool call remains protected on the same actor.

### Single turn

The generated single-turn tool runs through normal `BeforeTool`, `AfterTool`, and error callbacks.

Classify the call as `spawn: true`, bind the child with `ChildStart`, and report the returned function response through `SpawnResult`.

Derive a valid stable run ID from `toolCtx.FunctionCallID()`:

```go
sum := sha256.Sum256([]byte(toolCtx.FunctionCallID()))
runID := "appa-" + hex.EncodeToString(sum[:12])
```

Pass `runID` to `workflow.WithRunID`. Derive the OpenAPPA child ID from the same value.

Test empty, numeric-only, slash-bearing, at-sign-bearing, and repeated provider call IDs.

### Task

ADK task mode emits a no-op function tool callback before native deferred dispatch. OpenAPPA tool callbacks MUST skip configured task targets.

Use `AfterModelCallbacks` to inspect the model function call before `dispatchTaskFC` runs. Send `ToolCall` with `spawn: true` there.

Use the child `BeforeAgentCallbacks` to send `ChildStart` before the task child begins execution.

Use the runner plugin `OnEventCallback` before event persistence. Intercept the synthesized task function response and report `SpawnResult`.

Use `OnUserMessageCallback` to reject forged task function responses. Suppress task-scope child content until the gated parent return.

Test pause, resume, cancellation, duplicate callbacks, and process restart.

## Remote A2A children

The parent-side remote A2A call is a normal function tool. Existing tool callbacks gate its dispatch and return.

Require `isolateSessions: true` for a child trajectory.

Allocate the remote context ID during `BeforeTool`. Store it by function-call ID, then send `ChildStart` before the A2A request.

`remoteA2AState.handleFirstCall` MUST reuse that prepared context ID instead of creating one inside `tool.Run`.

Trusted deployment configuration MUST bind the authenticated remote endpoint to an OpenAPPA-capable Harness.

Change `remote_a2a_tool.processResult` to preserve `TaskState` in the tool result.

| A2A result | Parent behavior |
|---|---|
| Direct message | Report successful `SpawnResult` with content |
| `completed` | Report successful `SpawnResult` with content |
| `failed`, `canceled`, or `rejected` | Report failed `SpawnResult` without content |
| `input_required` or `auth_required` | Pause without a parent-facing result |
| `submitted` or `working` | Continue waiting or refuse. Do not return content |
| Unknown state | Refuse. Do not return content |

## Memory

Model-called `load_memory` uses normal tool callbacks.

Automatic `preload_memory` runs as a request processor and bypasses tool callbacks. Wrap `memory.Service` with an OpenAPPA decorator.

Before `Runner.Run`, put the acting trajectory ID in the invocation context. The decorator reads that ID before `Search`.

The decorator gates search before execution and admits returned memory before `PreloadMemoryTool` adds it to the model request.

Register background memory save, summarization, and embedding work as child work. Refuse it until the terminal return is observable.

## MCP and MCP Apps

HTTP, SSE, and stdio MCP tools become normal ADK function tools after discovery. Validate each dynamically resolved tool at dispatch.

Under the profile, MCP discovery and MCP App classification failures MUST fail closed. Do not use lazy fallback for unknown App metadata.

Model-called MCP App tools use normal tool callbacks. Their full result passes post-execution admission before UI publication or model compaction.

App-internal tools and resources bypass ADK. Route them back through the owning Actor instead of calling MCP directly from the controller.

Extend the tool and resource request protos with `agent_instance_id` and `trajectory_capability`.

The Actor mints an opaque random capability after the model-called App result passes admission. It stores the bound trajectory, actor, tool scope, and expiration.

The App UI returns that capability with each internal request. The controller resolves the AgentInstance and forwards the request to a new Actor gate RPC.

The Actor validates the capability, runs both OpenAPPA gates, executes the MCP call, and returns only admitted content.

Update these owners:

| Owner | Change |
|---|---|
| tools and resources protobufs | Add AgentInstance and capability fields |
| App UI client | Preserve and return the capability |
| `go/core/internal/grpcserver` | Stop direct execution for protected App requests |
| A2A or Actor control service | Add the private App gate RPC |
| Go ADK extension | Mint, store, validate, and expire capabilities |

Reject missing, expired, mismatched, or unknown capabilities before MCP dispatch.

## Long-running work

The profile supports long-running work only after registration as child work.

Google ADK exposes no callback for work that outlives the initiating callback. Add two optional callbacks to each kagent-owned background launcher and resume path:

| Proposed callback | Invocation point |
|---|---|
| `BeforeBackgroundStart` | Before kagent starts or resumes work and before any external effect |
| `OnBackgroundResult` | After terminal completion but before result publication or storage |

These are kagent function parameters, not user configuration and not a second middleware system.

When the OpenAPPA profile is active, a launcher MUST refuse work if either callback is absent.

Registration creates a stable child ID before launch. Completion, failure, cancellation, and resume use that same ID.

The terminal result MUST pass `SpawnResult` before model, parent, memory, UI, or event delivery.

Refuse streaming chunks, detached work, asynchronous MCP jobs, notifications, provider-native tools, and background memory until they use this lifecycle.

## Coverage catalog

### Supported after implementation

| Path | Required mechanism |
|---|---|
| Go ADK function tool | Final ADK argument snapshot and existing callbacks |
| HTTP, SSE, and stdio MCP tool | Dynamic validation and existing callbacks |
| Skill tool | Existing callbacks |
| Model-called memory tool | Existing callbacks |
| Automatic memory preload | Context-bound memory-service decorator |
| Local `chat` | Transfer gate on the parent trajectory |
| Local `single_turn` | Child binding and normal tool callbacks |
| Local `task` | `AfterModel` dispatch and `OnEvent` return gates |
| Remote A2A parent boundary | Normal callbacks, isolation, and preserved task state |
| Instrumented remote child | Compatible adapter and authenticated endpoint |
| Model-called MCP App tool | Normal callbacks before UI publication |
| `execute_remedy_plan` | Actor-bound vouch and local runtime MCP endpoint |
| MCP App internal call | Host gate and trajectory capability |
| Registered background work | Child identity and terminal return gate |
| Instrumented BYO agent | Compatible adapter and capability declaration |

### Refused in the first implementation

| Path | Missing mechanism |
|---|---|
| Streaming tool chunk | Chunk admission protocol |
| Detached or unregistered work | Stable child identity and terminal return |
| Unregistered asynchronous MCP job | Protected completion path |
| Notification-only result | Parent-facing return gate |
| Unregistered background memory | Protected completion path |
| Provider-native tool | Go ADK callbacks |
| Native `RequireApproval` | OpenAPPA authority and remedy integration |
| Additional short-circuit `BeforeTool` callback | Guaranteed OpenAPPA ordering and gate state |
| Uninstrumented remote child | Protected internal execution boundary |
| Uninstrumented Python ADK | Python adapter |
| Uninstrumented OpenAI Agents | Framework tool and handoff adapter |
| Uninstrumented LangGraph | Protected tool nodes and graph validation |
| Uninstrumented CrewAI | Protected tool and delegation layer |
| Uninstrumented BYO agent | Protected internal execution boundary |

## Upstream sequence

| Pull request | Generic kagent change | Proof |
|---|---|---|
| 1 | Snapshot final ADK arguments and call identity | Tool receives the value that policy authorized |
| 2 | Add extension-aware Go ADK builders | Existing call sites compile and keep behavior |
| 3 | Add local collaboration mode to AgentTemplate tools | All three modes compile and validate |
| 4 | Add task dispatch and return plugin seams | Deferred task cannot bypass either gate |
| 5 | Preserve remote A2A task state | Only valid terminal outcomes return content |
| 6 | Add result publication barriers | Unadmitted result reaches no consumer before callbacks finish |
| 7 | Add Harness callback and dynamic tool validation | Unknown or short-circuiting paths fail before execution |
| 8 | Add background-work lifecycle | Every registered result has one terminal gate |

The OpenAPPA Harness profile, policy compiler, Go ADK extension, and Rust adapter can remain integration-specific.

## Existing-cluster migration

`AgentInstance` pins one `Harness` and prepared revision. The implementation MUST NOT mutate a running instance into the OpenAPPA profile.

### Control-plane upgrade

1. Apply the generated CRD or Helm bundle with optional `KagentHarness.OpenAPPA` and collaboration-mode fields.
2. Deploy the patched kagent control-plane image.
3. Wait for every controller and conversion webhook to report ready.
4. Apply no OpenAPPA `Harness` until the patched control plane is ready.

Existing Harnesses omit the new field. Their compilation, builders, callbacks, and Actor images remain unchanged.

### Compatibility preparation

For each existing `AgentTemplate` selected for adoption:

1. Apply the policy `ConfigMap` and OpenAPPA `Harness`.
2. Let the compiler inventory tools, providers, child modes, MCP Apps, memory, and background paths.
3. Refuse preparation when any enabled path lacks required coverage.
4. Wait for a ready revision and Substrate `ActorTemplate` before creating an instance.

The same `AgentTemplate` can serve both the old and OpenAPPA Harnesses during rollout.

### State boundary

Create a new `AgentInstance`, Actor, `/data/openappa/appa.db`, and root trajectory family.

Do not copy an unprotected transcript into the OpenAPPA event log. The first release has no trustworthy facts for activity that happened before enforcement.

Persistent external records re-enter through policy-covered tools. Their tool contracts assign the resulting Labels.

If the application uses an external ADK session store, start a new protected session ID. Do not attach an old model context to the new root trajectory.

### Canary and cutover

The cluster operator owns routing, drain, retention, rollback, and destructive retirement.

1. Send synthetic and internal canary requests to the protected `AgentInstance`.
2. Verify runtime health, policy hash, callback coverage, denial, replacement, child return, and database persistence.
3. Route new root A2A tasks to the protected instance.
4. Keep continuations for old task and context IDs pinned to the old instance.
5. Drain until old work reaches a terminal state, or apply the documented timeout and cancellation policy.
6. Suspend the old instance, export its task and event records, and verify a Substrate snapshot containing `/data`.
7. Retain the export and snapshot for at least 30 days before any deletion.

Destructive deletion requires separate cluster-operator approval after the retention period.

### Rollback

Keep the old `AgentInstance` unchanged until cutover verification ends.

Rollback routes new root tasks to the old instance. Existing protected task and context IDs remain pinned to the protected instance until terminal or canceled.

After drain, suspend the protected instance, export its task and event records, and verify a Substrate snapshot containing `/data`.

Retain that export and snapshot for at least 30 days. Rollback does not merge, rewrite, or discard the OpenAPPA event log.

## Validation matrix

| Area | Required tests |
|---|---|
| Compatibility | Existing Harnesses, builders, CRDs, callback order, approval, and concurrency stay unchanged without the profile |
| Tool calls | Allow, deny, final argument snapshot, mutation attempt, serialization error, outcome, timeout, cancellation, and replacement |
| Callback bypass | Native approval, runner short-circuit, agent short-circuit, missing gate state, and safe refusal |
| Remedies | Actor binding, stale offer, authorized call, substituted bytes, returned value, decline, and no answer |
| Concurrency | Same-actor serialization, child parallelism, callback panic, and permit cleanup |
| Local agents | Chat transfer, single turn, task, pause, resume, forged response, cancellation, and restart |
| Remote A2A | Isolation, direct message, every task state, capability mismatch, and terminal replacement |
| Memory | Load, preload, save, context loss, background work, and replacement before model delivery |
| MCP Apps | Discovery failure, model tool, app-internal call, resource read, expired capability, and UI publication |
| Runtime | Startup health, process exit, database persistence, revision change, and fail-closed recovery |
| Egress | Every policy endpoint and explicit LLM URL enter the allowlist and revision digest |
| Migration | CRD ordering, dual revisions, task affinity, canary, cutover, drain, fresh session, suspend, export, snapshot, rollback, and retention |
| Browser | Proposal diagrams at desktop, mobile, light, and dark modes |

The implementation is complete only when each enabled execution path reaches its required pre-dispatch and result boundary.
