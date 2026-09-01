# kagent adapter implementation plan

Source baseline — the last stable kagent release:

- kagent [`v0.9.12`](https://github.com/kagent-dev/kagent/releases/tag/v0.9.12) (2026-07-20). This is the API the public kagent.dev docs describe: `kagent.dev/v1alpha2`, kind `Agent`.
- kagent-adk 0.3.0 (the workspace package version at that tag)
- google-adk 1.31.1 — the version the v0.9.12 lock resolves. The package constraint is `google-adk>=1.28.1,<2` ([pyproject.toml#L25](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/pyproject.toml#L25)). Every callback-semantics claim below is verified in the google_adk-1.31.1 wheel, and wheel citations give paths and lines inside it.
- OpenAPPA `origin/main`: `appa-runtime-api` hook vocabulary (`appa-runtime-api/src/lib.rs`), `appa-runtime` `/hook` endpoint (`appa-runtime/src/main.rs`), and the `appa-adapter-claude-code` codec as the adapter reference.

The reader-facing proposal is at [openappa.com/kagent](https://www.openappa.com/kagent). A forward section at the end covers kagent's unreleased `v1alpha3` cutover.

## Architecture decision

The adapter rides two stock kagent surfaces and changes no kagent or Google ADK source:

1. Helm `controller.agentImage.{registry,repository,tag,pullPolicy,pullSecret}` — the runtime image for every Declarative agent. The values render into the controller ConfigMap as `IMAGE_*` env ([controller-configmap.yaml#L12-L18](https://github.com/kagent-dev/kagent/blob/v0.9.12/helm/kagent/templates/controller-configmap.yaml#L12-L18)), which the controller consumes as `--image-*` flags. The adapter ships as that image.
2. `KAgentApp(plugins=[...])` — a public constructor parameter of the published `kagent-adk` package ([_a2a.py#L63](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L63)), forwarded into ADK's plugin manager ([_a2a.py#L124](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L124)). The `AppaHookPlugin` registers there.

No config-reachable plugin surface exists upstream. The stock entrypoint builds a closed plugin list ([cli.py#L69-L79](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L69-L79)). No CRD field, helm value, env var, or entry point adds to it. The adapter image therefore carries its own entrypoint. That entrypoint calls the same public `kagent-adk` functions as stock `static` and adds one plugin.

The adapter follows the `appa-adapter-claude-code` boundary. It maps harness callbacks to the eight `HookEvent` variants and renders each `HookDecision` back into the harness. It does not link `appa-runtime`, call the Engine, own policy, or open `appa.db`. `appa-runtime` owns policy, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db`.

## Artifacts and ownership

| Artifact | Contents | Owner |
|---|---|---|
| `appa-adapter-kagent` Python package | `AppaHookPlugin` (a google-adk `BasePlugin`) and `entrypoint.py` | OpenAPPA repository |
| `appa-adapter-kagent` OCI image | The published `kagent-dev/kagent/app` image plus the Python package, digest-pinned base | OpenAPPA repository |
| `appa-adapter-kagent` Rust crate | The `/hook` codec (`parse`, `render`) for the kagent wire, selected by the runtime's adapter switch | OpenAPPA repository |
| kagent | Controller, CRDs, translator, helm chart, `kagent-adk` library — all stock, no change | Upstream |
| Google ADK | Plugin API and dispatch loop — stock dependency, no change | Upstream |

The runtime picks one codec per adapter through a closed enum ([appa-runtime/src/main.rs](../../appa-runtime/src/main.rs), the `Adapter` enum). The kagent deliverable adds one variant beside `ClaudeCode`, mapped to `appa_adapter_kagent::codec()`.

The plugin holds no policy state. It serializes callback moments into wire events, sends them to `APPA_RUNTIME_URL`, and enforces the answered decision. Every semantic judgment stays in `appa-runtime`.

## Adapter workload image

Build: `FROM` the published `kagent-dev/kagent/app` image at a pinned digest, add the Python package, set `entrypoint.py` as the container entrypoint. `cli.py` stays in the image unchanged and unused. Equivalent build: a plain Python base plus `pip install kagent-adk==0.3.0`.

Runtime contract the image must keep — the controller sets container args, not the command:

- Accept `--host <bind> --port 8080 --filepath /config` ([deployments.go#L175-L179](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/deployments.go#L175-L179)).
- Read `config.json` and `agent-card.json` from the per-agent Secret mounted at `/config` ([manifest_builder.go#L243](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L243)).
- Serve A2A on port 8080 and answer readiness at `/.well-known/agent-card.json` ([manifest_builder.go#L532](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L532)).

Entrypoint steps, in order:

1. Parse the raw `/config/config.json` with a strict schema. Refuse any field the adapter does not support, and exit unready. Stock `AgentConfig` is a pydantic model that ignores unknown fields. The adapter must not inherit that silence.
2. Validate the accepted config with `AgentConfig.model_validate` and build the agent factory over `AgentConfig.to_agent`.
3. Rebuild the stock plugin list with the same conditions as `static` (STS token propagation, `LLMPassthroughPlugin`), then append `AppaHookPlugin(APPA_RUNTIME_URL)`.
4. Construct `KAgentApp(...)`, call `.build()`, and serve on the given host and port — the same calls as [cli.py#L88-L101](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L88-L101).

Refusal rules (fail closed at startup): `APPA_RUNTIME_URL` unset or unreachable at the startup probe — unready. Any config field outside the adapter's accepted schema — unready, with the field named in the log.

`APPA_RUNTIME_URL` delivery: a baked default in the image, overridable per agent through `spec.declarative.deployment.env` ([agent_types.go#L443-L445](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L443-L445)).

## AppaHookPlugin

The plugin implements google-adk `BasePlugin`. The 1.31.1 wheel defines 12 lifecycle callbacks (`base_plugin.py` lines 114, 136, 155, 174, 198, 217, 233, 253, 272, 297, 321, 348). Each gated callback sends one wire event to `POST $APPA_RUNTIME_URL/hook` and enforces the decision that comes back. A transport failure or a non-contract response raises, and ADK wraps the exception and aborts the invocation (`plugin_manager.py` 288-305).

| ADK callback | HookEvent | Enforcement at the callback | 1.31.1 wheel evidence |
|---|---|---|---|
| `on_user_message_callback`, first invocation of a fresh session | `SessionStart` | `Refuse` raises before `Prompt` is sent | `runners.py` 1537-1541 |
| `on_user_message_callback` | `Prompt` | `Block` raises pre-append, so the bytes never land in session history | fires before the append: `runners.py` 1537 then 1550-1556 |
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: return a dict — ADK skips execution, and the dict becomes the function response the model reads. `Refuse` raises. | `functions.py` 509-534, 588-592 |
| `after_tool_callback` | `ToolResult` | `ReplaceOutput{output}`: return a dict — it replaces the result the model sees. `Block` raises. The plugin recognizes its own deny dicts here (a denied call flows through this callback too) and reports them once. | `functions.py` 547-576 |
| `on_tool_error_callback` | `ToolResult` with `Failure` outcome | return a dict to convert, or re-raise. Does not fire for a `before_tool_callback` raise — a `Refuse` stays terminal. | `functions.py` 535-545 |
| `before_tool_callback` on an agent tool | `ToolCall{spawn:true}` | as `ToolCall`. The plugin holds the `AllowCall{spawn}` binding for the child scope | `functions.py` 509-534 |
| `before_agent_callback` (local sub-agent) | `ChildStart` | a returned `Content` ends the child before its body runs | `base_agent.py` 288-296, 447-452 |
| `after_tool_callback` on the agent-tool return | `SpawnResult` | `ChildReturn{value}` or `ReplaceOutput`: return a dict — the one point where the plugin can substitute the value the parent receives | `functions.py` 547-576 |
| `after_run_callback` | `TurnEnd` (root) | observe — also fires after a `before_run` halt | `runners.py` 843-861, 952 |
| `before_run_callback` | none | pass through — `Prompt` already gates the same bytes | `runners.py` 843-861 |
| `before_model_callback`, `after_model_callback`, `on_model_error_callback`, `on_event_callback` | none | liveness gates: raise when the `/hook` channel is down, pass otherwise | `base_plugin.py` 233, 253, 272, 155 |

`ChildEnd` is unfed by design. Return substitution is enforceable only where the parent receives the value, so returns cross at `SpawnResult`. The `appa-adapter-claude-code` parse map makes the same choice.

Error-turn gap: google-adk 1.31.1 has no `on_run_error_callback` and no `on_agent_error_callback` (absent from the wheel), and `after_run_callback` is skipped when a run dies on an unhandled error (`runners.py` 949-954, no `finally`). `on_model_error_callback` and `on_tool_error_callback` catch the common failures earlier. For the rest, `appa-runtime` recovery classifies the open dispatch as `Indeterminate` at the next admitted event, and the next `Prompt` fails closed if the runtime is down.

Sub-agent identity: a v1alpha2 agent declares another agent as a tool (`spec.declarative.tools[].type: Agent`), which kagent dispatches as `KAgentRemoteA2ATool` ([_remote_a2a_tool.py#L158-L170](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_remote_a2a_tool.py#L158-L170)). The plugin classifies a spawn by the tool's type. A successful child reply arrives as `{"result": ...}` for Task replies, and as a bare string for direct Message replies — the plugin handles both shapes. Local ADK sub-agents (BYO-authored) gate through `before_agent_callback`. An `AgentTool` child runs under a fresh child Runner that inherits the parent's plugin list, so coverage continues there.

### Wire and codec

The plugin emits one JSON event per callback: event kind, trajectory ids, tool name, raw argument bytes, outcome, and value fields — the data the matching `HookEvent` variant needs. The `appa-adapter-kagent` Rust crate parses this wire into `HookEvent` and renders each `HookDecision` into the response the plugin enforces, through the `Codec` contract of `appa-runtime-api` (`parse`, `render` — `appa-runtime-api/src/lib.rs`). The wire carries no policy meaning. Raw tool arguments cross as spelled, and the Engine canonicalizes them.

### Trajectory identity

- Root `TrajectoryId`: the ADK session id with a harness prefix, per the `appa-runtime-api` convention.
- Child classification: the child pod's plugin reads kagent's inbound call metadata to recognize a delegated entry. A delegated entry feeds `ChildStart`. A plain external entry feeds `SessionStart` and `Prompt`.
- Parent and child run as separate Deployments, so one trajectory's hooks come from two plugin instances. Both must reach the same `appa-runtime` — a per-pod runtime would split one trajectory into two logs.

## Deployment and rollout

```yaml
# helm values — the whole kagent-side change
controller:
  agentImage:
    registry: ghcr.io
    repository: archestra-ai/appa-adapter-kagent
    tag: "<adapter release>"
```

- Rollout is one `helm upgrade`. The controller re-renders every Declarative agent Deployment onto the adapter image, cluster-wide. There is no per-agent opt-in on this lane, and no double-run hazard.
- Staged rollout, when wanted: `spec.declarative.deployment.imageRegistry` overrides the registry component per agent ([agent_types.go#L392](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L392)) — point pilot agents at a registry that serves the adapter image under the stock repository path.
- `appa-runtime` deploys as one shared service per cluster or namespace. Only the runtime mounts policy, credentials, and the durable `appa.db` volume. Agent pods hold no APPA state.

## Known gaps and handling

| Gap | Handling |
|---|---|
| No callback at ADK session creation | `SessionStart` synthesizes at first invocation, sent before `Prompt` from the same callback. A never-invoked session emits nothing and flows nothing. |
| No error-turn callback in google-adk 1.31.1 | See the error-turn gap above: earlier error callbacks plus `Indeterminate` classification at recovery. |
| `runtime: go` agents (opt-in) | Out of scope on v0.9.12: no helm value selects the Go runtime image there, and the Go ADK's plugin list is compiled in. The stable default is `python` ([agent_types.go#L175](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L175)), so defaulted fleets are covered. |
| BYO agents (`spec.byo.deployment.image`) | Per-agent images outside any shared runtime image. Their authors add the one `plugins=[...]` line, or gate at MCP/model boundaries. |
| `AgentHarness` / `SandboxAgent` sandbox kinds | Different subsystem (Substrate sandboxes), out of scope. |
| Upstream has no plugin config knob | The entrypoint replays `static` behavior through public calls. CI pins kagent-adk and google-adk versions and runs an equivalence check on each bump. Propose an upstream plugin-loading knob to delete the duplication. |

## Forward lane: the v1alpha3 cutover (unreleased)

kagent's main branch replaces the v1alpha2 Agent controller with a `v1alpha3` `AgentTemplate` × `Harness` model compiled to Substrate Actors (merged to kagent main late August 2026, absent from every release and from public docs). On that lane the same adapter image lands in `Harness.spec.workload.image` (required, digest-pinned, [harness_types.go#L34-L40](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/api/v1alpha3/harness_types.go#L34-L40) at HEAD `52cc4de2`), with agents admitted per `allowedAgentTemplates` label selector and config injected as `KAGENT_CONFIG_JSON` env. The entrypoint already tolerates both config deliveries: `materialize_from_env` is a no-op when the env vars are absent. Two changes to track before that lane is real for users: the 0.10 release candidates flip the Declarative runtime default to `go`, and they add a `controller.goAgentImage` value — pointing it at the adapter image also serves go-declared agents from the Python runtime, except Foundry-model agents, which require the Go binary. Re-verify this section against the first stable 0.10/v1alpha3 release before acting on it.

## PR sequence

| PR | Change |
|---|---|
| 1 | `appa-adapter-kagent` Rust codec crate: wire parse to `HookEvent`, decision render, `Adapter` enum variant, unit tests against recorded wire fixtures |
| 2 | `appa-adapter-kagent` Python package: `AppaHookPlugin` with the callback table above, fail-closed transport, liveness gates, deny-dict self-recognition |
| 3 | Entrypoint: strict config schema and refusal rules, stock plugin parity, `KAgentApp` assembly, the `--host/--port/--filepath` args contract |
| 4 | OCI image build with pinned base digest, SBOM, and provenance |
| 5 | End-to-end harness: kind cluster with the v0.9.12 chart, `controller.agentImage` swap, parent-and-child scenario against one shared runtime |

No kagent PR and no Google ADK PR is required. The optional upstream contribution (a plugin config knob) is independent and non-blocking.

## Verification matrix

Adapter tests:

- Callback-to-event mapping for all rows of the table, including spawn classification by tool type and both child-return shapes (Task dict, bare Message string).
- Deny path: a `DenyCall` dict skips execution, reaches the model as the function response, and is not double-reported through `after_tool_callback`.
- Replace path: `ReplaceOutput` and `ChildReturn` substitution at `after_tool_callback`.
- Pre-append barrier: a blocked `Prompt` leaves no trace in session history.
- Fail closed: runtime down at each callback blocks the action, and liveness gates hold model and emission callbacks.
- Startup refusal: missing `APPA_RUNTIME_URL`, unknown config fields.
- Args contract: the entrypoint accepts the controller's `--host/--port/--filepath` args and answers readiness at `/.well-known/agent-card.json`.
- No link from the codec crate to `appa-runtime` or `appa-engine`, and no policy state in the plugin.

Equivalence tests:

- Entrypoint output matches stock `static` for the same `/config` contents, minus the added plugin: same agent shape, same stock plugins, same serving surface.
- Re-run on every kagent-adk and google-adk version bump. The callback table re-verifies against the newly locked google-adk.

End-to-end tests:

- Declarative agent on a kind cluster with the v0.9.12 chart and the `controller.agentImage` swap: gated tool calls, replaced results, blocked prompts.
- Parent and delegated child in separate pods against one shared runtime: one trajectory, `ChildStart` and `SpawnResult` correlated.
- Crash window: kill the agent pod between `ToolCall` and `ToolResult`, then make sure the runtime reports the dispatch `Indeterminate`.
- Error-turn window: force an unhandled model failure and make sure recovery closes the turn at the next admitted event.
