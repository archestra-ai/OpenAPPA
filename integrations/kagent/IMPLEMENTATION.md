# kagent adapter implementation plan

`appa-adapter-kagent` is the Rust codec crate: a workspace crate compiled into `appa-runtime`, selected by the runtime's closed `Adapter` enum ([appa-runtime/src/main.rs](../../appa-runtime/src/main.rs)) as `appa_adapter_kagent::codec()`. The agent side wraps kagent's ADK runtimes and takes its names from them. The `appa-kagent-adk` python package (plugin + entrypoint) ships as the `ghcr.io/archestra-ai/appa-kagent-adk` image. The `appa-kagent-adk-go` Go module (plugin + runtime main) ships as the `ghcr.io/archestra-ai/appa-kagent-adk-go` image. The crate name never names an image. Both images read `APPA_RUNTIME_URL`, both emit the same adapter wire, and the one codec crate parses it.

## Source baselines

Stable release — the lane users install today:

- kagent [`v0.9.12`](https://github.com/kagent-dev/kagent/releases/tag/v0.9.12) (2026-07-20), the `kagent.dev/v1alpha2` API the public docs describe.
- kagent-adk 0.3.0. google-adk 1.31.1 — the lock resolution. The constraint is `google-adk>=1.28.1,<2` ([pyproject.toml#L25](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/pyproject.toml#L25)). Python-side callback claims below are verified in the google_adk-1.31.1 wheel (wheel citations give paths and lines inside it).
- Go ADK: `google.golang.org/adk v1.4.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/go.mod#L50)) — the v1 line, before the v2 plugin API.

Release-candidate line — mid-cutover, in two observed states:

- Tag [`v0.10.0-rc4`](https://github.com/kagent-dev/kagent/releases/tag/v0.10.0-rc4) (`af84a618`, 2026-08-26): still the v1alpha2 `Agent` → Deployment controller, plus the `controller.goAgentImage` value. Go ADK: `google.golang.org/adk/v2 v2.1.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/go.mod#L50)). The workspace lock resolves google-adk 2.8.0.
- Main at [`52cc4de2`](https://github.com/kagent-dev/kagent/commit/52cc4de2a044a5062d10c4f189d863937c1bb0f9) (2026-09-01): the v1alpha2 Agent controller is deleted, and agents are `v1alpha3` `AgentTemplate` × `Harness` pairs compiled to Substrate Actors. Go ADK: `google.golang.org/adk/v2 v2.2.0`.

OpenAPPA: `appa-runtime-api` hook vocabulary (`appa-runtime-api/src/lib.rs`) and the `appa-runtime` `/hook` endpoint (`appa-runtime/src/main.rs`).

## Architecture decision

No kagent fork and no Google ADK fork, in either runtime:

- **Python runtime**: `KAgentApp(plugins=[...])` is a public constructor parameter of the published `kagent-adk` package ([v0.9.12 _a2a.py#L63](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L63)), forwarded into ADK's plugin manager. kagent registers its own plugins through it ([cli.py#L69-L79](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L69-L79)). The stock entrypoint's plugin list is closed and no config adds to it, so the adapter image carries its own entrypoint that makes the same public calls and appends one plugin.
- **Go runtime**: kagent's Go runtime is Google's official Go ADK. On the release-candidate line it registers plugins through the ADK v2 plugin API — kagent's own runner adapter passes `runner.PluginConfig{Plugins: ...}` ([v0.10.0-rc4 go/adk/pkg/runner/adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)) — but the list is compiled in, with no config knob. The Go adapter is therefore a replacement runtime main built on kagent's public `go/adk` packages that registers `AppaHookPlugin` (Go) in that list.
- Delivery is always an image reference the operator already controls. The lanes below name the knob per tree.

Both plugins hold no policy state. They serialize callback moments into wire events, send them to `APPA_RUNTIME_URL`, and enforce the answered `HookDecision`. `appa-runtime` owns policy, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db`.

## Delivery lanes

### Lane A — stable release (v1alpha2 `Agent` → Deployment)

The shipped controller reconciles every `Agent` into a plain Deployment + Service. The declarative runtime image is install configuration:

- **Python (the stable default runtime)**: `spec.declarative.runtime` defaults to `python` in the shipped CRD ([agent_types.go#L175](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L175)). Helm `controller.agentImage.{registry,repository,tag}` flows into the controller ConfigMap as `IMAGE_*` ([controller-configmap.yaml#L12-L18](https://github.com/kagent-dev/kagent/blob/v0.9.12/helm/kagent/templates/controller-configmap.yaml#L12-L18)) and lands as the agent Deployment's image. Pointing it at `appa-kagent-adk` gates every python-runtime agent with zero agent changes.
- **Go**: v0.9.12 has no Go-image value and no Go-image controller flag, and its Go ADK is the v1 line without the v2 plugin API. Go-runtime agents are opt-in there. To gate one on stable, set `runtime: python` on that Agent (one field) — or move to lane B, which carries the Go adapter.

Runtime contract the python image must keep — the controller sets container args, not the command:

- Accept `--host <bind> --port 8080 --filepath /config` ([deployments.go#L175-L179](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/deployments.go#L175-L179)).
- Read `config.json` and `agent-card.json` from the per-agent Secret mounted at `/config` ([manifest_builder.go#L243](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L243)).
- Serve A2A on port 8080 and answer readiness at `/.well-known/agent-card.json` ([manifest_builder.go#L532](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L532)).

Rollout: one `helm upgrade` re-renders every declarative python agent onto the adapter image, cluster-wide, with no double-run hazard. Staged rollout: `spec.declarative.deployment.imageRegistry` overrides the registry component per agent ([agent_types.go#L392](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L392)) — point pilot agents at a registry that serves the adapter image under the stock repository path. `APPA_RUNTIME_URL` arrives as a baked image default or per agent via `spec.declarative.deployment.env` ([agent_types.go#L443-L445](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L443-L445)).

### Lane B — release-candidate line

Two observed states, one adapter story:

**B1 — the current release candidates (v1alpha2 `Agent` → Deployment, both image knobs).** Same Deployment path as lane A, with three differences. The runtime default flips to `go` ([rc4 agent_types.go#L235-L241](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/api/v1alpha2/agent_types.go#L235-L241)). `controller.goAgentImage.{registry,repository,tag}` exists beside `controller.agentImage` ([rc4 controller-configmap.yaml#L28](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/helm/kagent/templates/controller-configmap.yaml#L28), [app.go#L226](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/pkg/app/app.go#L226)). And agents with skills or `executeCodeBlocks` resolve a `<tag>-full` Go-image variant, so the Go adapter publishes both tags. Point both values at the matching images and every declarative agent is gated — python agents by `appa-kagent-adk`, go agents by `appa-kagent-adk-go`. Foundry-model agents require the Go runtime by compiler validation ([rc4 compiler.go#L224-L227](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/compiler.go#L224-L227)). The Go adapter covers them, because it is a Go ADK runtime.

**B2 — main (v1alpha3 `AgentTemplate` × `Harness` → Substrate Actor).** The Harness names the runtime image directly and selects which templates it runs:

```yaml
kind: Harness
spec:
  kagent: {}
  workload:
    image: ghcr.io/archestra-ai/appa-kagent-adk@sha256:<digest>
  env:
    - name: APPA_RUNTIME_URL
      value: http://appa-runtime.appa-system:8787
  substrate: { workerPoolRef: {name: <pool>}, snapshotPolicy: {location: <loc>} }
  allowedAgentTemplates:
    selector: { matchLabels: { <team labels> } }
```

- `workload.image` is required and digest-pinned ([harness_types.go#L34-L40](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/api/v1alpha3/harness_types.go#L34-L40)). `spec.env` carries `APPA_RUNTIME_URL`, with Secret refs available.
- Pairing: the controller matches each `AgentTemplate` against every same-namespace Harness selector and reconciles one Actor per pair ([collections.go#L85-L102](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/controller/collections.go#L85-L102)). Rollout is "move the label match", never "add a second match" — a template matched by two Harnesses runs twice. Make old and new selectors disjoint.
- Config arrives as `KAGENT_CONFIG_JSON` / `KAGENT_AGENT_CARD_JSON` env ([actor_template.go#L43-L44](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L43-L44)). The python entrypoint's `materialize_from_env` handles both deliveries and is a no-op on the Deployment path. The Substrate path caps an Actor at 32 total env vars ([actor_template.go#L50-L52](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L50-L52)), and the adapter needs one.
- Prerequisites: helm `controller.substrate.enabled=true`, the `ate-system` install, and a `WorkerPool` — the stock Substrate-path requirements.
- Templates whose compiled config carries in-process `sub_agents` ([kagent/compiler.go#L172](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/kagent/compiler.go#L172)) refuse at the python entrypoint (stock parsing drops them silently — the adapter must not). Out-of-process (`Dedicated`) sub-agent tools are rejected upstream ([compiler.go#L149-L151](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/compiler.go#L149-L151)). The Go adapter consumes the Go-shaped config natively and carries these topologies once its mapping verifies.
- Re-verify this sub-lane against the first release that ships v1alpha3 before acting on it.

## Runtime adapters

### Python — `AppaHookPlugin` on google-adk (verified)

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

`ChildEnd` is unfed by design. Return substitution is enforceable only where the parent receives the value, so returns cross at `SpawnResult`.

Per-ADK differences the plugin handles:

- **google-adk 1.31.1 (stable lane)**: no `on_run_error_callback` and no `on_agent_error_callback` (absent from the wheel), and `after_run_callback` is skipped when a run dies on an unhandled error (`runners.py` 949-954, no `finally`). The model-error and tool-error callbacks catch the common failures earlier. For the rest, `appa-runtime` recovery classifies the open dispatch as `Indeterminate` at the next admitted event. An `AgentTool` child runs under a fresh child Runner that inherits the parent's plugin list, so coverage continues there.
- **google-adk 2.8.0 (release-candidate lane lock)**: both error callbacks exist — the plugin feeds `TurnEnd` (root failure) from `on_run_error_callback` and `TurnEnd` (child failure) from `on_agent_error_callback`, closing the error-turn gap on that lane. Sub-agents re-enter `run_async`, so `before/after_agent_callback` fire per child directly.

Entrypoint (python image), in order:

1. Parse the mounted or env-delivered config with a strict schema, and refuse unknown fields with an unready exit.
2. Validate with `AgentConfig.model_validate` and build the factory over `to_agent`.
3. Rebuild the stock plugin list with the stock conditions (STS token propagation, LLM passthrough), then append `AppaHookPlugin(APPA_RUNTIME_URL)`.
4. Construct `KAgentApp(...)`, call `.build()`, and serve on the controller-given host and port — the same calls as [cli.py#L88-L101](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L88-L101).

### Go — `AppaHookPlugin` on the Go ADK (design, verification pending)

The Go adapter is a small runtime main, module `appa-kagent-adk-go`, that imports kagent's public `go/adk` packages, constructs the same agent the stock Go runtime builds from the rendered config, and registers the Go `AppaHookPlugin` through the ADK v2 plugin API — the registration point kagent itself uses ([rc4 adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)). It emits the same adapter wire as the python plugin.

Verification status: the callback-to-hook mapping above is proven for the python plugin against its pinned google-adk versions. The Go mapping must be proven the same way against `google.golang.org/adk/v2` at the locked version (v2.1.0 on rc4, v2.2.0 on main) before the Go image ships: which plugin callbacks exist, whether a before-tool return skips execution and reaches the model as the function response, whether an after-tool return replaces the result, and where the user message crosses into session state. The Go runtime also serves Foundry-model agents, which the compiler ties to the Go runtime. Deliverables: both tags (`<tag>` and `<tag>-full`, the variant kagent resolves for agents with skills or `executeCodeBlocks`).

## Wire and codec

Each plugin emits one JSON event per callback: event kind, trajectory ids, tool name, raw argument bytes, outcome, and value fields — the data the matching `HookEvent` variant needs. The `appa-adapter-kagent` Rust crate parses this wire into `HookEvent` and renders each `HookDecision` into the response the plugins enforce, through the `Codec` contract of `appa-runtime-api` (`parse`, `render`). The wire carries no policy meaning. Raw tool arguments cross as spelled, and the Engine canonicalizes them. One wire and one codec serve both runtime images.

## Trajectory identity

- Root `TrajectoryId`: the ADK session id with a harness prefix, per the `appa-runtime-api` convention.
- Child classification: the child pod's plugin reads kagent's inbound call metadata to recognize a delegated entry (on the v1alpha3 lane the executor lands it in session state — [_agent_executor.py#L212-L214](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L212-L214)). A delegated entry feeds `ChildStart`. A plain external entry feeds `SessionStart` and `Prompt`.
- A successful child reply arrives as `{"result": ...}` for Task replies and as a bare string for direct Message replies — the plugins handle both shapes.
- Parent and child run as separate workloads (Deployments on lane A/B1, Substrate Actors on B2), so one trajectory's hooks come from two plugin instances. Both must reach the same `appa-runtime` — a per-pod runtime would split one trajectory into two logs.

## Known gaps and handling

| Gap | Lane / runtime | Handling |
|---|---|---|
| No callback at ADK session creation | all | `SessionStart` synthesizes at first invocation, sent before `Prompt`. A never-invoked session emits nothing and flows nothing. |
| No error-turn callback in google-adk 1.31.1 | A / python | Earlier error callbacks plus `Indeterminate` classification at recovery. Closed on lane B by google-adk 2.8.0's error callbacks. |
| Go-runtime agents on stable | A / go | No Go-image knob and a v1 Go ADK in v0.9.12: flip those agents to `runtime: python` (one field), or adopt lane B. |
| Go mapping unproven | B / go | Verify against the locked `adk/v2` before shipping the Go image. Until then the Go lane is design, not a claim. |
| CRD in-process `sub_agents` on the python runtime | B2 / python | The entrypoint refuses the config instead of dropping children. The Go adapter consumes them natively once verified. |
| BYO agents | all | Per-agent images outside any shared runtime image. Their authors add the one plugin line, in either language. |
| Sandbox kinds (`AgentHarness`, `SandboxAgent`) | all | Different subsystem, out of scope. |
| Upstream has no plugin config knob | all | The entrypoints replay stock behavior through public calls. CI pins kagent and ADK versions per lane and re-runs the equivalence checks on each bump. Propose an upstream plugin-loading knob to delete the duplication. |

## PR sequence

| PR | Change |
|---|---|
| 1 | `appa-adapter-kagent` Rust codec crate: wire parse to `HookEvent`, decision render, `Adapter` enum variant, unit tests against recorded wire fixtures |
| 2 | `appa-kagent-adk` Python package: `AppaHookPlugin` with the callback table, per-ADK deltas, fail-closed transport, liveness gates, deny-dict self-recognition |
| 3 | Python entrypoint: strict config schema and refusal rules, stock plugin parity, both config deliveries, the controller args contract |
| 4 | Python OCI image with pinned base digest, SBOM, and provenance |
| 5 | Lane A end-to-end: kind cluster with the stable chart, `controller.agentImage` swap, parent-and-child scenario against one shared runtime |
| 6 | `appa-kagent-adk-go`: adk/v2 mapping verification, the Go plugin and runtime main, both image tags |
| 7 | Lane B end-to-end: the B1 dual-knob swap on the release-candidate chart, and B2 Harness × AgentTemplate on the Substrate path |

No kagent PR and no Google ADK PR is required. The optional upstream contribution (a plugin config knob) is independent and non-blocking.

## Verification matrix

Adapter tests (per runtime):

- Callback-to-event mapping for every table row, including spawn classification by tool type and both child-return shapes.
- Deny path: a `DenyCall` skips execution, reaches the model as the function response, and is not double-reported.
- Replace path: `ReplaceOutput` and `ChildReturn` substitution at the after-tool point.
- Pre-append barrier: a blocked `Prompt` leaves no trace in session history.
- Fail closed: runtime down at each callback blocks the action, and liveness gates hold model and emission callbacks.
- Startup refusal: missing `APPA_RUNTIME_URL`, unknown config fields, `sub_agents` on the python runtime.
- Args contract: the entrypoints accept the controller's args and answer readiness at the stock endpoint.
- No link from the codec crate to `appa-runtime` or `appa-engine`, and no policy state in either plugin.

Equivalence tests:

- Each entrypoint's output matches its stock counterpart for the same rendered config, minus the added plugin.
- Re-run on every kagent and ADK version bump, per lane. The callback tables re-verify against the newly locked ADK.

End-to-end tests:

- Lane A: declarative python agent on a kind cluster with the stable chart and the `controller.agentImage` swap — gated tool calls, replaced results, blocked prompts.
- Lane B1: both image knobs swapped on the release-candidate chart — a python agent and a go agent gated side by side.
- Lane B2: `AgentTemplate` × `Harness` on the Substrate path — admission by selector, `KAGENT_CONFIG_JSON` delivery, the env-var cap respected.
- Cross-workload trajectory: parent and delegated child against one shared runtime — one trajectory, `ChildStart` and `SpawnResult` correlated.
- Crash window: kill the agent workload between `ToolCall` and `ToolResult`, then make sure the runtime reports the dispatch `Indeterminate`.
- Error-turn window per lane: on lane A, force an unhandled model failure and make sure recovery closes the turn at the next admitted event. On lane B, make sure the error callbacks feed the failure `TurnEnd`s.
