# adk/v2 mapping verification — cells B1-go and B2-go

The go table in [IMPLEMENTATION.md](../IMPLEMENTATION.md) shipped as
design with its behavior column marked "verification pending". This
file closes that obligation against the locked sources on disk:

- `google.golang.org/adk/v2` **v2.1.0** — all `adk-go` citations below
  are file:line in that tag's source tree.
- `github.com/kagent-dev/kagent/go` at commit **`af84a618`** (the tree
  tagged `v0.10.0-rc4`) — cited as `kagent go/...`.

Every plugin behavior in `plugin.go` cites its proof here. The tests in
`plugin_test.go` and `wire_test.go` exercise each verified row against
a scripted `/hook` server and the shared wire fixtures; all pass on
go 1.26.7 (see the test section of the PR).

## The mapping table, row by row

| Go callback | Plan claim | Status | Evidence (adk-go v2.1.0) |
|---|---|---|---|
| `OnUserMessageCallback` | fires before the session append, and a returned error aborts the run | **VERIFIED** | `runner/runner.go:617-631` runs the callback and returns its error before the user event is built and appended at `runner/runner.go:650-667`. Both run paths abort on that error: the node path at `runner/run_node.go:97-101`, the classic path at `runner/runner.go:288-292`. The blocked bytes never land in session history. |
| `BeforeToolCallback` | a non-nil map skips execution and reaches the model as the function response | **VERIFIED** | `internal/llminternal/base_flow.go:1236-1246`: a non-nil map (or error) from the plugin skips both the agent-level callbacks and `tool.Run`. The map becomes the `FunctionResponse` the model reads at `base_flow.go:1173-1187`. Caveat below on the error form. |
| `BeforeToolCallback` on `execute_remedy_plan` | `PassControl`: a nil map lets the call through to `/mcp` | **VERIFIED** | `base_flow.go:1240-1246`: `(nil, nil)` falls through to the agent-level callbacks and then `tool.Run`. The reserved tool is an ordinary mcptoolset tool, so the call proceeds untouched. |
| `AfterToolCallback` | a non-nil map replaces the result the model sees | **VERIFIED** | `base_flow.go:1261-1272`: a non-nil map from the plugin replaces the response before the function-response event is built. Caveat below: unlike python, this point also runs on error paths. |
| `OnToolErrorCallback` | a map converts the error, and a returned error stays terminal | **VERIFIED** | `base_flow.go:1248-1259`: a returned map becomes the response and clears the error; a returned error replaces it. Also invoked for tool-not-found (`base_flow.go:1153-1158`). "Terminal" caveat below. |
| `BeforeAgentCallback` | a returned `Content` ends the child before its body runs | **VERIFIED** | `agent/agent.go:250-265`: non-nil content becomes an event and calls `ctx.EndInvocation()`; `agent/agent.go:182-191` yields it and returns before `a.run` executes. A returned error aborts the scope (`agent/agent.go:251-254`). |
| `AfterAgentCallback` | fires once per sub-agent scope | **VERIFIED, with a skip window** | `agent/agent.go:206-209` runs it once per `agent.Run` completion, and every agent (root and sub-agent) runs through that wrapper. Skip window: `agent/agent.go:202-204` — when the invocation ended during the body (`EndInvocation`, or a before-agent halt), the after callbacks never run, per the documented contract at `agent/agent.go:131-137`. A child scope that ends the invocation therefore emits no `turn_end`; `appa-runtime` recovery closes the turn at the next admitted event, as on the error-turn gap. |
| `AfterRunCallback` | observation only — the signature returns no value | **VERIFIED** | `plugin/plugin.go:165`: `type AfterRunCallback func(agent.InvocationContext)`. Deferred at `runner/runner.go:298`, `runner/run_node.go:111`, and `runner/runner.go:504`, so it also fires after a `BeforeRunCallback` halt. It does not fire when `OnUserMessageCallback` aborts (the defer is registered after the append), so a blocked prompt leaves no dangling `turn_end` — nothing was admitted. |
| `BeforeRunCallback` | liveness gate | **VERIFIED** | `runner/runner.go:300-313` and `runner/run_node.go:113-127`: a returned error is yielded and the run body never starts. |
| `BeforeModelCallback`, `AfterModelCallback`, `OnModelErrorCallback` | liveness gates | **VERIFIED** | `internal/llminternal/base_flow.go:755-775` (a before-model error skips the model call and fails the step), `:890-910`, `:912-932`. `OnModelErrorCallback` answering `(nil, nil)` lets the original model error propagate (`base_flow.go:785-793`). |
| `OnEventCallback` | liveness gate | **VERIFIED** | `runner/runner.go:332-343` and `runner/run_node.go:171-182`: an error is yielded in place of the event and the event is neither persisted nor delivered; the kagent executor treats a stream error as run failure (`kagent go/adk/pkg/a2a/executor.go:267-271`). |
| plugin registration | `runner.PluginConfig{Plugins: ...}` is the registration point | **VERIFIED** | kagent builds the list at `kagent go/adk/pkg/runner/adapter.go:93-112`; adk-go feeds it into the plugin manager at `runner/runner.go:104-110`. |
| plugin order | no stock plugin answers a gated callback, so appending last is safe | **VERIFIED for rc4** | the manager stops each chain at the first non-nil answer (`internal/plugininternal/plugin_manager.go`, every `Run*` loop). The only stock go plugin is the STS token-propagation plugin, and it wires exactly `BeforeRunCallback` and `AfterRunCallback` (`kagent go/adk/pkg/sts/plugin.go:305-309`) — neither is a gated callback. An STS before-run early exit could at most skip the appa before-run liveness ping; every flow-carrying event still gates. Re-verify on each kagent bump. |

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
`lineageRoot`) exactly as the python twin does: root header first,
parent header as fallback, pinned at first classification. So a
delegated child pod on this image opens `child_start`, the fork binds
it, and its later events carry the parent's root id
(`main_test.go`, `TestTheLineageHeadersLandInSessionStateOnGetAndCreate`).
The decorator is per request because the kagent session service does
not fold `state_delta` on `Get`; landing the headers once at `Create`
would leave every later `Get` without them.

## Callback contexts on adk-go

**Finding: the tool and agent callbacks never see the session.**
adk-go hands `BeforeToolCallback`/`AfterToolCallback`/`OnToolErrorCallback`
a tool context and `BeforeAgentCallback`/`AfterAgentCallback` a
callback context, and both refuse `Session()` and `Agent()` — each
logs `is not supported for callback context` and returns nil
(`agent/common_context.go`). Only the run-level `InvocationContext`
(`OnUserMessage`, `BeforeRun`, `OnEvent`, `AfterRun`) carries the
session. Both contexts do answer `InvocationID()` and `AgentName()`.

The plugin therefore pins the trajectory ids when the run opens
(`openInvocation`, from `OnUserMessage` and `BeforeRun`) and every
later callback looks them up by `InvocationID()` (`idsFor`); `AfterRun`
forgets them. A callback whose invocation was never pinned fails
closed. The current agent's name comes from `AgentName()`. A first
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

In-process sub-agents need no spawn entry on these cells: the rc4 go
`AgentConfig` has no sub-agent field (`kagent go/api/adk/types.go:586-600`),
and in-tree delegation would cross as `transfer_to_agent`, an ordinary
`ToolCall{spawn:false}`, plus `ChildStart` from the per-scope agent
callbacks — the plan's stated mapping.

## The runtime main replays stock construction

`cmd/appa-kagent-adk-go/main.go` is the stock
`kagent go/adk/cmd/main.go` (rc4) with four marked deltas, using only
exported packages — the module imports no kagent `internal/` package
(`go/adk/pkg/...` and `go/api/adk` only; verified by `go build`, which
would refuse an internal import from an outside module).

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
   (above). Classification is then python-identical.
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
