# adk/v2 mapping verification — cells A-go, B1-go and B2-go

This file verifies the go table in [IMPLEMENTATION.md](../IMPLEMENTATION.md)
against the locked sources on disk:

- `google.golang.org/adk/v2` **v2.1.0** — all `adk-go` citations below
  are file:line in that tag's source tree.
- `github.com/kagent-dev/kagent/go` at commit **`af84a618`** (the tree
  tagged `v0.10.0-rc4`) — cited as `kagent go/...`.

That baseline is the one `go.mod` locks. The plan names the image
built from it for every go cell, and only cell A-go runs. On cell A-go
(kagent v0.9.12) the image runs under the `golang-adk` name the
controller derives from `controller.agentImage`. Both demo matrix rows
for that cell pass 18/18 after the per-parent child-return work
([../e2e/README.md](../e2e/README.md)). No
matrix row runs cell B1-go (v0.10.0-rc4) or cell B2-go (kagent main,
adk/v2 v2.2.0). This file does not verify the B2 baseline or its
configuration semantics.

Every plugin behavior in `plugin.go` cites its proof here. The tests in
`plugin_test.go` and `wire_test.go` exercise each verified row against
a scripted `/hook` server and the shared wire fixtures. They pass on
go 1.26.7 (`go test ./...`).

## The mapping table, row by row

| Go callback | Plan claim | Status | Evidence (adk-go v2.1.0) |
|---|---|---|---|
| `OnUserMessageCallback` | fires before the session append, and a returned error aborts the run | **VERIFIED** | `runner/runner.go:617-631` runs the callback and returns its error before the user event is built and appended at `runner/runner.go:650-667`. Both run paths abort on that error: the node path at `runner/run_node.go:97-101`, the classic path at `runner/runner.go:288-292`. The prompt bytes of an aborted run never land in session history. |
| `BeforeToolCallback` | a non-nil map skips execution and reaches the model as the function response | **VERIFIED** | `internal/llminternal/base_flow.go:1236-1246`: a non-nil map (or error) from the plugin skips both the agent-level callbacks and `tool.Run`. The map becomes the `FunctionResponse` the model reads at `base_flow.go:1173-1187`. Caveat below on the error form. |
| `BeforeToolCallback` on `execute_remedy_plan` | `PassControl`: a nil map lets the call through to `/mcp` | **VERIFIED** | `base_flow.go:1240-1246`: `(nil, nil)` falls through to the agent-level callbacks and then `tool.Run`. The reserved tool is an ordinary mcptoolset tool, so the call proceeds untouched. |
| `AfterToolCallback` | a non-nil map replaces the result the model sees | **VERIFIED** | `base_flow.go:1261-1272`: a non-nil map from the plugin replaces the response before the function-response event is built. Caveat below: unlike python, this point also runs on error paths. |
| `OnToolErrorCallback` | a map converts the error, and a returned error stays terminal | **VERIFIED** | `base_flow.go:1248-1259`: a returned map becomes the response and clears the error; a returned error replaces it. Also invoked for tool-not-found (`base_flow.go:1153-1158`). "Terminal" caveat below. |
| `BeforeAgentCallback` | a returned `Content` ends the child before its body runs | **VERIFIED** | `agent/agent.go:250-265`: non-nil content becomes an event and calls `ctx.EndInvocation()`; `agent/agent.go:182-191` yields it and returns before `a.run` executes. A returned error aborts the scope (`agent/agent.go:251-254`). |
| `AfterAgentCallback` | fires once per sub-agent scope | **VERIFIED, with a skip window** | `agent/agent.go:206-209` runs it once per `agent.Run` completion, and every agent (root and sub-agent) runs through that wrapper. Skip window: `agent/agent.go:202-204` — when the invocation ended during the body (`EndInvocation`, or a before-agent halt), the after callbacks never run, per the documented contract at `agent/agent.go:131-137`. A child scope that ends the invocation therefore emits no `turn_end`. `appa-runtime` recovery closes an unreported dispatch at the next turn end, or at the first tool call after the next prompt. The error-turn gap closes the same way. |
| `AfterRunCallback` | observation only — the signature returns no value | **VERIFIED** | `plugin/plugin.go:165`: `type AfterRunCallback func(agent.InvocationContext)`. Deferred at `runner/runner.go:298`, `runner/run_node.go:111`, and `runner/runner.go:504`, so it also fires after a `BeforeRunCallback` halt. It does not fire when `OnUserMessageCallback` aborts (the defer is registered after the append), so an aborted prompt leaves no dangling `turn_end`: nothing entered the session. |
| `BeforeRunCallback` | liveness gate | **VERIFIED** | `runner/runner.go:300-313` and `runner/run_node.go:113-127`: a returned error is yielded and the run body never starts. |
| `BeforeModelCallback`, `AfterModelCallback`, `OnModelErrorCallback` | liveness gates in a root scope, and the return gate in a child scope ([the section below](#the-childs-return-gate-on-adk-go)) | **VERIFIED** | `internal/llminternal/base_flow.go:755-775` (a before-model error skips the model call and fails the step), `:890-910`, `:912-932`. `OnModelErrorCallback` answering `(nil, nil)` lets the original model error propagate (`base_flow.go:785-793`). |
| `OnEventCallback` | liveness gate | **VERIFIED** | `runner/runner.go:332-343` and `runner/run_node.go:171-182`: an error is yielded in place of the event and the event is neither persisted nor delivered; the kagent executor treats a stream error as run failure (`kagent go/adk/pkg/a2a/executor.go:267-271`). |
| plugin registration | `runner.PluginConfig{Plugins: ...}` is the registration point | **VERIFIED** | kagent builds the list at `kagent go/adk/pkg/runner/adapter.go:93-112`; adk-go feeds it into the plugin manager at `runner/runner.go:104-110`. |
| plugin order | no stock plugin answers a gated callback, so appending last is safe | **VERIFIED for rc4** | the manager stops each chain at the first non-nil answer (`internal/plugininternal/plugin_manager.go`, every `Run*` loop). The only stock go plugin is the STS token-propagation plugin, and it wires exactly `BeforeRunCallback` and `AfterRunCallback` (`kagent go/adk/pkg/sts/plugin.go:305-309`) — neither is a gated callback. An STS before-run early exit could at most skip the appa before-run liveness ping. Every flow-carrying event still gates. |

### Error-path caveats the python table does not have

These are go-ADK mechanics, not gaps; `plugin.go` handles each one, and
`plugin_test.go` pins the handling.

1. **A `BeforeToolCallback` error does not abort the invocation.** In
   python, a raise aborts the run. In go, `callTool` never returns an
   error: a final error becomes the `{"error": ...}` function response
   the model reads (`base_flow.go:1274-1276`). Fail-closed still holds
   — the tool never executes — but the run continues with the refusal
   text as the tool's answer.
2. **The error flows on through the later tool callbacks.** An error
   from the before-tool point reaches `OnToolErrorCallback`
   (`base_flow.go:1250-1252`) and then `AfterToolCallback`
   (`base_flow.go:1261-1272`, which go runs "regardless of whether the
   tool returned a result or an error", `agent/llmagent/llmagent.go:392-399`).
   The plugin therefore self-recognizes: `onToolError` passes through
   its own `FailClosedError` without a second wire event, and
   `afterTool` passes through every error path — the failure already
   crossed at `onToolError` for real tool errors.
3. **A deferred result reaches the after-tool point.** A long-running
   or response-deferring tool yields `(nil, nil)` from `tool.Run`, and
   go still runs `AfterToolCallback` with a nil result. The python
   ADK has no such call. The plugin reports it as an `indeterminate`
   outcome — the dispatch is genuinely unresolved at that moment.

## Trajectory identity on the go runtime

**Finding: the go executor does not land the inbound lineage headers
in session state; the runtime main lands them itself.** The stock
executor persists exactly two keys when it creates a session:
`session_name` and `source` (`kagent go/adk/pkg/a2a/executor.go:184-201`,
`kagent go/adk/pkg/a2a/consts.go:6-7`). The
`x-kagent-root-context-id` / `x-kagent-parent-context-id` headers are
stamped on **outbound** delegated calls
(`kagent go/adk/pkg/tools/remote_a2a_tool.go:49-50, 79-107`) and are
readable inbound only from the live A2A call context
(`a2asrv.CallContextFrom(ctx).RequestMeta()`), never from state.

`main.go` closes the gap with `lineageSessionService`, a decorator over
the stock session service. On `Create` and on `Get` it reads the two
headers from the call context of the request being served and sets
them under the python-shaped `"headers"` state key, in memory, on the
session it hands back. The plugin reads that key (`plugin.go`,
`lineageRoot`) as the python twin does: root header first, parent
header as fallback. It classifies from the headers as landed before
each run (`classify`, from `openInvocation`), never from a first-sight
pin. So a delegated child pod on this image opens `child_start`, the
fork binds it, and its later events carry the parent's root id
(`main_test.go`, `TestTheLineageHeadersLandInSessionStateOnGetAndCreate`).
The decorator is per request because the kagent session service does
not fold `state_delta` on `Get`; landing the headers once at `Create`
would leave every later `Get` without them.

**Finding: the go remote-agent tool sends every delegation from a pod
into one child context.** `NewKAgentRemoteA2ATool` mints
`sharedContextID` once, at construction
(`kagent go/adk/pkg/tools/remote_a2a_tool.go:199, 211`).
`contextIDForCall` returns it for every call while `isolateSessions`
is false (`remote_a2a_tool.go:152-164, 227-234`). Each call sends that
id as the message context id, with the parent session id in the call
context (`remote_a2a_tool.go:306-316`). The flag comes from
`RemoteAgentConfig.IsolateSessions` (`kagent go/api/adk/types.go:437-442`,
`kagent go/adk/pkg/agent/agent.go:59`), false by default. The v0.9.12
tree has no such field, so the v1alpha2 CRD cannot set it on cell
A-go. The child pod therefore sees one ADK session id for every parent
that delegates into it.

The python twin never shares one. The kagent python executor builds a
fresh runner per A2A request from the root agent factory, and the
remote tool it builds mints its child context id at construction
(kagent-adk 0.3.0 `_agent_executor.py:128-137`, `_a2a.py:111-112`,
`_remote_a2a_tool.py:177, 324`).

Each parent opens the child under its own root id, so no parent takes
the fork of another. The child trajectory id carries the root id
(`appa-adapter-kagent/src/lib.rs`, `child`), and `bind_child` binds a
start to the fork of that root
(`appa-runtime/src/api/session.rs`). `openScope` therefore sends
`child_start` for each (root, child) pair it has not opened, and the
pair — not the session — is what decides.

The plugin keeps the pairs it opened (`opened`), exactly as the python
twin does (`plugin.py`, `_opened`). A re-entry of an opened pair sends
no second `child_start`: the runtime ended the child trajectory when
its first return crossed the parent's gate, and the child context id
can bind no second fork (the limit below). The re-entry then runs in
the ended trajectory, and the log line names that case. A pair joins
the set only after the runtime acked, so a refused start opens nothing
and the next entry sends `child_start` again. A root session keeps the
`isFresh` rule for `session_start`.

**Limit: on kagent v0.9.12 one go parent delegates into a given child
once per parent session.** The go tool sends a second delegation from
the same parent session into the same child context id, and no header
carries a per-delegation discriminator. That second delegation prepares
a second fork, and a child bound to one fork binds no other
(`appa-runtime/src/api/session.rs`, `bind_child`, and
`EngineRefusal::Unbindable` mapped to `BindingMismatch` in
`appa-runtime/src/api/mod.rs`). The child resumes under the first fork,
and its stop crosses under the return policy of that fork. The second
spawn result of the parent then comes back blocked with `the fork and
the child are already bound elsewhere` (`on_spawn_result`, the
`SpawnPlan::Bind` arm on a bound child). The rc4 `isolateSessions`
field mints one context per call and removes the limit. The python cell
mints a fresh child context per parent request and has no such limit.
Both matrices delegate once per parent session, so no matrix row
observes the limit.

`TestEachParentOpensTheSharedChildSessionUnderItsOwnRoot` and
`TestARootSessionStillOpensOnceAtItsFirstContent` (`plugin_test.go`)
pin the two opening cases. A second delegated entry of the same pair
sends only its prompt (`TestTheSameParentSendsNoSecondChildStart`), and
a refused start opens again on the next entry
(`TestARefusedChildStartFailsClosedAndTheNextEntryOpensAgain`).

## The child's return gate on adk-go

**Finding: adk-go carries the three levers a forced return gate
needs.** A child returns at its own stop, so the plugin holds that stop
and posts `child_end` there. Three mechanics make that possible at
v2.1.0, and `plugin.go` uses only them.

1. **The request is rebuilt per step, and its tool map is writable.**
   `runOneStep` builds a fresh `model.LLMRequest`
   (`internal/llminternal/base_flow.go:552-562`), `preprocess` fills
   `req.Tools` from the tools of the agent (`base_flow.go:691-704`),
   and `LLMRequest.Tools` is a plain `map[string]any`
   (`model/llm.go:31-38`). The plugin registers its return-gate tool on
   every step.
2. **The plugin sees that request before the model call.** `callLLM`
   runs `RunBeforeModelCallback` with the request it is about to send
   (`base_flow.go:755-762`), and the callback takes a
   `*model.LLMRequest` (`agent/llmagent/llmagent.go:366`).
3. **An after-model answer replaces the response, and the replacement
   reaches the tool dispatch.** The plugin callbacks run first
   (`base_flow.go:890-898`), a non-nil answer becomes the yielded
   response (`base_flow.go:804-820`), and `handleFunctionCalls`
   resolves each call from `req.Tools`
   (`base_flow.go:596-623, 1041-1091`). A function-response event is
   not a final response, so `Flow.Run` calls the model again
   (`base_flow.go:103-130`).

So the go plugin replaces the final text of a child with one call to
its own return-gate tool. The body of that tool posts `child_end` and
enforces the answer. An `ack` lets the stop stand. A `child_return`
comes back as a second `child_end` carrying those exact bytes. A
`block` comes back as the tool result, the model writes another final
message, and the after-model point holds that stop too. The plugin
matches its own gate tool by identity at `BeforeToolCallback` and at
`AfterToolCallback` — the pointer it built, never the name — and it
posts nothing at either point (the section below). The reserved tool is
the other case: the plugin posts its `ToolCall`, and the runtime
absorbs that call by name.

The gate tool is `appa_return`, the name the python twin registers
(`plugin.py`, `RETURN_TOOL`). It is a plain struct with
`Name`/`Description`/`IsLongRunning`, a `Declaration()`, and
`Run(agent.Context, any) (map[string]any, error)` — the shape adk-go's
own synthetic tool carries
(`internal/llminternal/outputschema_processor.go:112-146`). adk-go's
`appendTools` is package-private
(`internal/llminternal/agent_transfer.go:258-304`), so `beforeModel`
does what it does: it writes the tool into `req.Tools` and appends its
declaration to the first `genai.Tool` of `req.Config`.

**The gate takes that slot and never yields it.** `preprocess` fills
`req.Tools` from the agent's own tools before the plugin's model point
(`base_flow.go:691-704`), so a tool of the gate's name — an MCP toolset
with no `tool_filter`, a remote agent so named — is already in the slot
when `beforeModel` runs. `registerReturnGate` overwrites the entry
unless it is the gate itself, and drops any declaration of that name
the request carried, so the model reads one `appa_return` and it is the
gate's own. Leaving the entry alone is the whole bypass: the held stop
is dispatched out of `req.Tools` by name
(`base_flow.go:596-623, 1041-1091`), so the child's entire final answer
would go to that tool, ungated and unreported; no `child_end` would
post; and because nothing crossed, every later stop would synthesize
the same call again. `TestAToolsetCannotTakeTheGatesSlotInARealRunner`
runs that case in the adk/v2 loop — with the overwrite reverted it
records ten `tool_call` events into the foreign tool and no `child_end`
before the test's event bound stops it.

The consequence is worth naming: in a child scope a tool of that name
is shadowed, because the gate holds the slot for every step. In a root
scope the plugin registers no gate, so that tool is callable and it
crosses the tool gate like any other. Either way the config guard
refuses the collision at the start, so a gated agent never runs with a
tool the operator declared and cannot call.

`TestAChildScopeStopsThroughTheReturnGateInARealRunner`
(`plugin_test.go`) runs the whole hold in the adk/v2 loop — a scripted
`model.LLM`, an `llmagent`, the in-memory session service, and the
plugin registered through `runner.PluginConfig`. The child speaks, the
plugin replaces that stop with the gate call, adk-go dispatches the
call from `req.Tools`, `child_end` crosses, and the child's reply
carries the bytes that crossed. That run is the executable proof of the
three levers above.

**The parent declares the return of a spawn itself.** A `deny_call`
whose offers carry a return route never reaches the model. The plugin
takes the bare-floor offer (`returns: as_spoken`), posts a synthetic
`tool_call` for `execute_remedy_plan` with `{offer_id, label: {}}` to
earn the vouch, runs that plan on `$APPA_RUNTIME_URL/mcp`, and posts
the identical `tool_call` again. It declares once per call: a runtime
that does not answer `pass_control` hands the block back to the model
with its menu, and a second deny goes to the model as it stands. The
`/mcp` leg speaks MCP through `github.com/modelcontextprotocol/go-sdk`
(`mcp.StreamableClientTransport`, `CallTool`), the same client adk-go's
`mcptoolset` uses. `go.mod` still marks that module `// indirect`; the
import is now direct and the marker is stale.

**A `context` answer at a child's start rides the first user message.**
kagent carries no side channel for the return contract, so `openScope`
returns the text and `onUserMessage` prepends it as the first part of
the message the child reads. The request the parent sent stands
unchanged, and the `prompt` event carries the parent's text alone. A
`context` at a root session start is an answer outside the contract of
that event, and it fails closed.

**What the go cell cannot do.** No end-of-child callback can hold a
child at its stop. `AfterRunCallback` returns no value
(`plugin/plugin.go:165`), and `AfterAgentCallback` gets a callback
context that refuses `Session()` (`agent/common_context.go`, the
section above). The model points are the only seam, so a child whose
model produces no further final message ends with nothing crossed. A
`HookDecision::Context` answer has no channel in a local in-process
scope either: a `Content` returned from `BeforeAgentCallback` ends the
child before its body runs (`agent/agent.go:250-265`), so `beforeAgent`
takes `ack` alone. A delegated entry has the channel (above). Both
images refuse in-process sub-agents, so no gated config reaches that
scope.

## What the plugin decides from

**Finding: a name and a payload key are both writable from outside the
plugin, so neither decides anything.** Two skip paths in the tool gate
used to read one: the gate's name, and the `appa` marker in a result
map. Both now key on identity the plugin owns.

**The gate, by pointer.** `beforeTool` and `afterTool` skip exactly
`p.returnTool`, the object `New` built (`isReturnGate`, a type
assertion plus a pointer comparison). A tool that merely carries the
name crosses both points like any other tool. It can exist: an MCP
toolset with no `tool_filter` loads whatever its server advertises
(`tool/mcptoolset`), and in a root scope the plugin registers no gate
of its own to shadow it. Under a name comparison that tool skipped the
call gate and the result gate both — `beforeTool` returned nil before
any wire event, the call ran, and `afterTool` reported nothing, so a
whole flow left the scope with no event at all.
`TestAToolNamedAfterTheGateIsGatedLikeAnyOther` pins the tool_call and
tool_result that now cross.

**The plugin's own answers, by function-call id.** The deny map and the
pending-review map the plugin hands the model carry `"appa": "denied"`
and `"appa": "review"`, and the model reads them. The after-tool point
no longer decides from those bytes: `beforeTool` records the
function-call id of the call it answered itself, and `afterTool` skips
only a call in that set, dropping the entry as it reads it. Any tool
can write those bytes — an MCP server sets its own result fields, and
adk-go hands the result to the plugin verbatim — and under the old
check such a result skipped the result gate, so bytes reached the model
with the runtime holding no record of them and every later `permits`
check ran on a label missing that source.
`TestAToolResultThatCarriesAnAppaMarkerStillCrosses` pins all three
markers crossing as ordinary results.

**`FunctionCallID` is populated at both tool points, and it is the same
value.** `handleFunctionCalls` builds one tool context per call —
`agent.NewToolContext(toolCallCtx, fnCall.ID, ...)`
(`base_flow.go:1071-1074`) — and `callTool` hands that one context to
the before-tool point, the tool, the error point and the after-tool
point (`base_flow.go:1232-1272`). The id is never empty by the time it
gets there: the genai API leaves `FunctionCall.ID` optional and some
models never set it, so adk-go generates one
(`utils.PopulateClientFunctionCallID`, `internal/utils/utils.go:37-50`)
in `finalizeModelResponseEvent` (`base_flow.go:958-963`), which runs on
the yielded response — the plugin's own after-model replacement
included — before `handleFunctionCalls` reads it. Confirmed in the
adk/v2 loop with a probe on both callbacks: the ordinary call and the
synthesized gate call each carried one `adk-<uuid>` id at the
before-tool and after-tool points.
`TestTheCallThePluginAnsweredIsRecognizedAtTheAfterToolPointInARealRunner`
runs a real deny through that loop; a plugin that recorded no id posts
a `tool_result` for a dispatch the runtime never opened, and the test
fails on the extra event. A call whose id is empty records nothing and
is reported like any other result — the safe direction, and no gate is
skipped.

**The rendered config refuses the collision at the start.**
`decodeGuarded` refuses a config that declares `appa_return` as an MCP
tool (`http_tools[i].tools[j]`, `sse_tools[i].tools[j]`) or as a remote
agent (`remote_agents[i].name`), and the diagnostic names the position
(`configguard.go`, `refuseReservedToolNames`). The operator reads the
collision at the start rather than meeting it at the first delegation.
The check runs before the ignored-value checks, and only for a gated
agent, like every other refusal. It cannot see a toolset with an empty
filter, whose server can still advertise the name; that case is the
plugin's, and the two paragraphs above hold it. The engine's own
`execute_remedy_plan` is not in the refused set: the runtime main
appends that toolset itself, after the guard runs.

## Callback contexts on adk-go

**Finding: the tool and agent callbacks never see the session.**
adk-go hands `BeforeToolCallback`/`AfterToolCallback`/`OnToolErrorCallback`
a tool context and `BeforeAgentCallback`/`AfterAgentCallback` a
callback context, and both refuse `Session()` and `Agent()` — each
logs `is not supported for callback context` and returns nil
(`agent/common_context.go`). Only the run-level `InvocationContext`
(`OnUserMessage`, `BeforeRun`, `OnEvent`, `AfterRun`) carries the
session. Both contexts do answer `InvocationID()` and `AgentName()`,
and the tool context also answers `FunctionCallID()` — populated at
every tool point, and one value per call
([above](#what-the-plugin-decides-from)).

The plugin therefore pins the trajectory ids when the run opens
(`openInvocation`, from `OnUserMessage` and `BeforeRun`) and every
later callback looks them up by `InvocationID()` (`idsFor`); `AfterRun`
ends the turn under them and then forgets them. The pin reads the
session state of that run, so every callback inside one run, the turn
end included, carries one (root, child) pair, and the next run
classifies afresh
(`TestAnOpenedInvocationKeepsItsIdsWhenTheHeadersChangeMidRun`). A
gated callback whose invocation was never pinned fails closed; a turn
end with no pin classifies from the session as it reads then. The current agent's name comes from `AgentName()`. A first
image that read `Session()` from the tool context panicked on every
turn on a live cluster; `TestToolAndAgentCallbacksNeedNoSessionOrAgentOnTheirContext`
drives the whole turn through a context that refuses both accessors.

## The reviewed call's pending response

**Finding: adk-go yields the reviewed control call's pending response
before the confirmation event; python yields it after.** Both ADKs
build a function response for a `before_tool` result and a separate
`adk_request_confirmation` call when the callback requested a
confirmation. Python yields the confirmation event first and the
response second (`google/adk/flows/llm_flows/base_llm_flow.py`, the
`tool_confirmation_event` branch), and kagent's python executor stops
converting at the confirmation event (`kagent/adk/_agent_executor.py`,
the `long_running_tool_ids` break), so the python task history holds
the call and the confirmation, never the pending response. adk-go
yields the response first (`internal/llminternal/base_flow.go`,
`handleFunctionCalls`), and the go executor breaks only at the
confirmation, so the go task history showed the reviewed
`execute_remedy_plan` as completed. The kagent dashboard renders the
approval card only for a call without a response: the go cell showed
"Awaiting approval..." and no Approve/Reject.

`main.go` closes the gap at the same boundary python drops the event:
`reviewShapedExecutor` wraps the stock executor's event queue and
drops the plugin's own pending-review response part (recognized by
`IsPendingReview`: the reserved tool's response carrying the plugin's
marker) from every status update; an update left with no parts is not
written. ADK's session keeps the event, as it does on python
(`main_test.go`, `TestThePendingReviewResponseNeverReachesTheTask`).

## Spawn classification

**Deviation from the python twin, forced by the go tree.** Python
classifies agent-as-tool calls by class name (`AgentTool`,
`KAgentRemoteA2ATool`). The go runtime has no such type:
`NewKAgentRemoteA2ATool` returns a generic `functiontool`
(`kagent go/adk/pkg/tools/remote_a2a_tool.go:199-226`), the same
concrete type as any function tool. Classification is therefore
name-based: the runtime main derives the spawn-tool names from
`AgentConfig.RemoteAgents` — the same source the stock builder wires
the tools from (`kagent go/adk/pkg/agent/agent.go:53-65`), skipping
the same no-URL entries — and hands them to the plugin as
`Config.SpawnTools`. The spawn-return shapes match python's:
`functiontool` converts the tool's `remoteA2AResponse` struct into a
map with `result` and `subagent_session_id`
(`adk-go tool/functiontool/function.go:227-246`,
`kagent go/adk/pkg/tools/remote_a2a_tool.go:173-181`).

The rc4 go `AgentConfig` has no sub-agent field
(`kagent go/api/adk/types.go:586-600`). Its decoder
(`types.go:618-669`, `kagent go/adk/pkg/config/config_loader.go:21`)
drops a `sub_agents` key at load. So the go main refuses a config that
declares `sub_agents` before the stock decoder runs (`configguard.go`,
exit 1). The same guard refuses `agent_plugins` and any other top-level
key outside the rc4 schema. On the decoded config it refuses
`execute_code` true and a `context_config` that is not null (below).
No in-process sub-agent runs on this baseline. The plugin classifies
spawns by the configured remote-agent names only, so a
`transfer_to_agent` call crosses as an ordinary
`ToolCall{spawn:false}`. A sub-agent scope that opens in-process sends
`child_start` with no spawn in flight, and the runtime refuses it
(`SpawnNotTaken`, `appa-runtime/src/api/session.rs`). No test exercises
`transfer_to_agent`. `TestARefusedChildScopeFailsClosed`
(`plugin_test.go`) exercises only the plugin's fail-closed handling of a
scripted `refuse` on that `child_start`, not the runtime's refusal.

## The runtime main replays stock construction

`cmd/appa-kagent-adk-go/main.go` is the stock
`kagent go/adk/cmd/main.go` (rc4) with seven marked deltas, the ones its
header comment numbers. The first six are the `APPA_RUNTIME_URL`
refusal, the reserved-tool toolset, the plugin appended last, the
reasoning-effort fill, the lineage-header session service, and the
review-shaped executor. The seventh is the config guard
(`configguard.go`), which refuses a rendered config this image cannot
run as declared: a key outside the rc4 schema, or a value the go
runtime would ignore. It uses only exported packages: the module
imports no kagent `internal/` package (`go/adk/pkg/...` and
`go/api/adk` only). `go build` verifies that, because it refuses an
internal import from an outside module.

- The stock main itself uses only exported packages
  (`kagent go/adk/cmd/main.go:15-25`), so the replay is exact.
- The reserved-tool toolset rides the stock `HttpTools` path:
  an appended `HttpMcpServerConfig` becomes a streamable-HTTP
  `mcptoolset` through `mcp.CreateToolsets`
  (`kagent go/adk/pkg/agent/agent.go:50`,
  `kagent go/adk/pkg/mcp/registry.go:92-125, 338-343`).
- The OpenAI reasoning effort fills from
  `APPA_KAGENT_OPENAI_REASONING_EFFORT` onto `*adk.OpenAI.ReasoningEffort`
  (`kagent go/api/adk/types.go:80-91`, the `*string` the stock translator
  writes) only when the rendered config left it nil — so a CRD-set
  value wins. The v1alpha2 `ModelConfig` enum has no `none`, which some
  OpenAI models require for function tools on chat completions.
- The human-review channel, as the python plugin carries it: a
  `deny_call`'s `review` is remembered; a reserved call quoting a
  reviewed offer raises ADK's tool confirmation from the plugin's own
  `BeforeToolCallback` — adk-go hands that callback the tool context
  (`base_flow.go:1076,1238`; `agent/common_context.go:114,392-409`), the
  same seam kagent's stock approval gate uses (`go/adk/pkg/agent/approval.go`)
  — and the resumed call reads `ToolConfirmation()` into `ruling`.
  `BeforeModelCallback` strips the `adk_request_confirmation` parts
  from the model's view. The reserved toolset's request timeout is
  300 s (`remedyCallTimeoutSeconds`).
- The config guard reads the raw `config.json` once, after
  `config.MaterializeFromEnv`
  (`kagent go/adk/pkg/config/config_materialize.go:22`), so one read
  covers the mounted file and the `KAGENT_CONFIG_JSON` delivery. It
  refuses `sub_agents`, `agent_plugins`, and any other top-level key
  outside the rc4 `adk.AgentConfig` json tags
  (`kagent go/api/adk/types.go:586-600`). It then decodes the bytes it
  checked through the stock decoder (`AgentConfig.UnmarshalJSON`,
  `types.go:618-669`). That decoded config is the one the runtime
  runs. The main never calls `config.LoadAgentConfigs`
  (`kagent go/adk/pkg/config/config_loader.go:44`), which reopens the
  same pathname. After the guard it calls only
  `config.ValidateAgentConfigUsage` (`config_usage.go:48`) on the
  decoded config and `config.LoadAgentCard` (`config_loader.go:29`)
  for the card. On the decoded config the guard refuses a tool
  declared under an APPA-owned name — `appa_return` in an MCP tool
  filter or as a remote-agent name, the collision the section
  [above](#what-the-plugin-decides-from) describes — then
  `execute_code` true (`types.go:610-616`) and every `context_config`
  that is not null, the empty object included (`types.go:543`). The
  go runtime builds neither a code executor nor context compaction,
  so it would run the agent without them. kagent's reconciler warns
  on the same two features and renders them anyway
  (`kagent go/core/internal/controller/reconciler/reconciler.go:941-955`).
  Its context warning fires only when the CRD sets
  `context.compaction` (`reconciler.go:947`), and the compiler renders
  `context_config: {}` for a `context` block without one
  (`kagent go/core/internal/controller/translator/agent/compiler.go:262-305`).
  So the guard refuses a wider set than the reconciler reports: a
  `context: {}` block gets no controller warning and still exits 1
  here. The `network` allowlist passes. Neither runtime reads it from
  `config.json` at the pinned versions: the go side only logs
  `hasNetworkConfig` (`config_usage.go:79`), and the python side only
  declares the field (v0.9.12 `types.py:370-384`). The controller
  renders the same allowlist into `srt-settings.json`
  (`kagent go/core/internal/controller/translator/agent/manifest_builder.go:273,375-379`),
  and the go skills shell applies it from there
  (`kagent go/adk/pkg/skills/shell.go:111-124`, called from
  `adk/pkg/tools/skills.go:117` and `adk/pkg/skills/skills_tools.go:45`).
  A refusal exits 1 with one diagnostic line that names the key.
  `TestTheConfigGuardRefusesWhatThisImageCannotRunAsDeclared`
  (`main_test.go`) pins the key and value tables. It pins that an
  accepted config equals what the stock decoder decodes from the same
  bytes, and that a config the decoder cannot decode surfaces as the
  decoder's own error.
  `TestTheRuntimeRefusesToStartOnAnUnsupportedConfig` runs the built
  binary. It pins exit 1 and the key on stderr for a mounted
  `config.json`, a `KAGENT_CONFIG_JSON` delivery, and a `-filepath`
  dir. It pins the `execute_code` and `appa_return` refusals inside
  the binary, and that an accepted config passes the stock validation
  and reaches the agent card load.
- Readiness: `app.New` serves the agent card at
  `a2asrv.WellKnownAgentCardPath` = `/.well-known/agent-card.json`
  (`kagent go/adk/pkg/a2a/server/server.go:42-45`), and the
  `--host/--port/--filepath` args plus the `KAGENT_CONFIG_JSON` /
  `KAGENT_AGENT_CARD_JSON` env delivery are the stock ones
  (`kagent go/adk/cmd/main.go:59-88`,
  `kagent go/adk/pkg/config/config_materialize.go:22-36`).

## Module resolution (go.sum outcome)

The plan named the requirement "`github.com/kagent-dev/kagent/go`,
tag `go/v0.10.0-rc4` → version `v0.10.0-rc4`". **That tag does not
exist upstream**: the repository tags `v0.10.0-rc4` at the root only,
and go modules in a subdirectory need a `go/`-prefixed tag to carry a
semantic version. The module therefore resolves as the pseudo-version
at the tagged commit:

```
github.com/kagent-dev/kagent/go v0.0.0-20260826134133-af84a618cb6b
```

`af84a618cb6bf91a249988131d62e15ecbaa5285` is exactly the commit the
`v0.10.0-rc4` tag points at, fetched unmodified through the module
proxy and locked by `go.sum` — the same tree, under the only version
spelling the go toolchain accepts for it. `google.golang.org/adk/v2`
resolves as the plan states, at `v2.1.0`. The kagent module declares
`go 1.26.7`, so the build toolchain is go 1.26.7 (auto-selected).

Module path: `github.com/archestra-ai/OpenAPPA/integrations/kagent/appa-kagent-adk-go`,
as planned; go tooling accepted it unchanged. The root package is
`appakagentadk` (go package names cannot carry hyphens).

## Deviations from the plan, summarized

1. Kagent go module pinned by pseudo-version at the rc4 commit — the
   planned tag spelling does not exist upstream (above).
2. Spawn classification by configured tool name, not type name — no
   distinctive type exists in the go tree (above).
3. The lineage headers land in session state through the runtime
   main's session-service decorator, not through the go executor
   (above). Classification reads the same headers. Both plugins open a
   (root, child) pair once and suppress the repeat, and each parent
   opens the child under its own root id, because the go remote-agent
   tool shares one child context across every parent of a pod (above).
4. `beforeAgent`/`afterAgent` distinguish the invocation's own scope
   by first-seen agent name per invocation id, where python compares
   against `callback_context.agent_name`. The go `agent.Context` has
   no invocation-agent accessor distinct from the current agent
   (`agent/context.go`, `agent/common_context.go:305`), so the plugin
   records the first scope each invocation opens and clears the entry
   at `afterRun`. Same observable behavior: own scope pings, later
   differently-named scopes open `child_start`.
5. A deferred (long-running) result crosses as an `indeterminate`
   outcome — a callback moment the python ADK never delivers (caveat 3
   above). The wire already carries the status; the codec parses it.
6. The `/mcp` leg of the return declaration imports
   `github.com/modelcontextprotocol/go-sdk/mcp` directly, where the
   python cell imports the python MCP client inside the function.
   `go.mod` carries that module as an indirect requirement of adk-go
   and still marks it `// indirect`; the marker needs one line of
   `go.mod` to drop. Nothing else changes: `go build`, `go vet` and
   `go test` resolve the import from the same locked version.
