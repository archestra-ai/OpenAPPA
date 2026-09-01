# kagent adapter implementation plan

Source baseline:

- kagent commit [`52cc4de2a044a5062d10c4f189d863937c1bb0f9`](https://github.com/kagent-dev/kagent/commit/52cc4de2a044a5062d10c4f189d863937c1bb0f9) (2026-09-01)
- google-adk 2.8.0, the version kagent's workspace lockfile resolves. The `kagent-adk` package constraint is `google-adk[a2a,db]>=2.6.2,<3` ([pyproject.toml#L26](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/pyproject.toml#L26))
- Substrate v0.0.20 ([go.mod#L489](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/go.mod#L489))
- OpenAPPA `origin/main`: `appa-runtime-api` hook vocabulary (`appa-runtime-api/src/lib.rs`), `appa-runtime` `/hook` endpoint (`appa-runtime/src/main.rs`), and the `appa-adapter-claude-code` codec as the adapter reference.

The reader-facing proposal is at [openappa.com/kagent](https://www.openappa.com/kagent). ADK citations below give paths and line numbers inside the published `google_adk-2.8.0` wheel.

## Architecture decision

The adapter rides two stock kagent surfaces and changes no kagent or Google ADK source:

1. `Harness.spec.workload.image` — a required, operator-supplied, digest-pinned OCI reference ([harness_types.go#L34-L40](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/api/v1alpha3/harness_types.go#L34-L40)). The adapter ships as that image.
2. `KAgentApp(plugins=[...])` — a public constructor parameter of the published `kagent-adk` package ([_a2a.py#L65](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L65)), forwarded into ADK's `App(plugins=...)` and its `PluginManager` ([_a2a.py#L136-L139](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L136-L139)). The `AppaHookPlugin` registers there.

No config-reachable plugin surface exists upstream. The stock entrypoint builds a closed plugin list ([cli.py#L95-L105](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/cli.py#L95-L105)). No CRD field, helm value, env var, or entry point adds to it. The adapter image therefore carries its own entrypoint. That entrypoint calls the same public `kagent-adk` functions as stock `static` and adds one plugin.

The adapter follows the `appa-adapter-claude-code` boundary. It maps harness callbacks to the eight `HookEvent` variants and renders each `HookDecision` back into the harness. It does not link `appa-runtime`, call the Engine, own policy, or open `appa.db`. `appa-runtime` owns policy, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db`.

## Artifacts and ownership

| Artifact | Contents | Owner |
|---|---|---|
| `appa-adapter-kagent` Python package | `AppaHookPlugin` (a google-adk `BasePlugin`) and `entrypoint.py` | OpenAPPA repository |
| `appa-adapter-kagent` OCI image | The published `kagent-adk` image plus the Python package, digest-pinned | OpenAPPA repository |
| `appa-adapter-kagent` Rust crate | The `/hook` codec (`parse`, `render`) for the kagent wire, selected by the runtime's adapter switch | OpenAPPA repository |
| kagent | Controller, CRDs, compiler, Substrate path, `kagent-adk` library — all stock, no change | Upstream |
| Google ADK | Plugin API and dispatch loop — stock dependency, no change | Upstream |

The runtime picks one codec per adapter through a closed enum ([appa-runtime/src/main.rs](../../appa-runtime/src/main.rs), the `Adapter` enum). The kagent deliverable adds one variant beside `ClaudeCode`, mapped to `appa_adapter_kagent::codec()`.

The plugin holds no policy state. It serializes callback moments into wire events, sends them to `APPA_RUNTIME_URL`, and enforces the answered decision. Every semantic judgment stays in `appa-runtime`.

## Adapter workload image

Build: `FROM` the published `kagent-adk` image at a pinned digest, add the Python package, set `entrypoint.py` as the container entrypoint. `cli.py` stays in the image unchanged and unused. Equivalent build: a plain Python base plus `pip install kagent-adk` at the locked version.

Entrypoint steps, in order:

1. Call `materialize_from_env("/config")` — the public `kagent-adk` function that writes `KAGENT_CONFIG_JSON` and `KAGENT_AGENT_CARD_JSON` to config files and expands `__KAGENT_ENV[...]__` credential placeholders ([_config_materialize.py#L55-L69](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_config_materialize.py#L55-L69)). The controller injects both variables into the Actor ([actor_template.go#L43-L44](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L43-L44)).
2. Parse the raw config JSON with a strict schema. Refuse any field the adapter does not support, and exit unready. Stock `AgentConfig` is a pydantic model that ignores unknown fields ([types.py#L387-L401](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/types.py#L387-L401)). The adapter must not inherit that silence. See the refusal rules below.
3. Validate the accepted config with `AgentConfig.model_validate` and build the agent factory over `AgentConfig.to_agent` ([types.py#L403](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/types.py#L403)).
4. Rebuild the stock plugin list with the same conditions as `static` (STS integration, `LLMPassthroughPlugin`), then append `AppaHookPlugin(APPA_RUNTIME_URL)`.
5. Construct `KAgentApp(...)`, call `.build()`, and serve — the same calls as [cli.py#L114-L135](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/cli.py#L114-L135).

Refusal rules (fail closed at startup):

- `APPA_RUNTIME_URL` unset, or the runtime unreachable at startup probe — unready.
- Config contains compiled in-process sub-agents (`sub_agents`). The kagent compiler emits them for CRD sub-agent tools ([kagent/compiler.go#L172](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/kagent/compiler.go#L172)), and Python `AgentConfig` drops them silently. The adapter refuses the config instead. Out-of-process (`Dedicated`) sub-agent tools do not reach this path — the compiler rejects them upstream ([compiler.go#L149-L151](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/compiler.go#L149-L151)).
- Config contains any other field outside the adapter's accepted schema — unready, with the field named in the log.

## AppaHookPlugin

The plugin implements google-adk `BasePlugin`. All 14 lifecycle callbacks exist in the 2.8.0 wheel (`google/adk/plugins/base_plugin.py`, lines 114-394). Each gated callback sends one wire event to `POST $APPA_RUNTIME_URL/hook` and enforces the decision that comes back. A transport failure or a non-contract response raises, and ADK aborts the invocation.

| ADK callback | HookEvent | Enforcement at the callback | ADK evidence (2.8.0 wheel) |
|---|---|---|---|
| `on_user_message_callback`, first invocation of a fresh session | `SessionStart` | `Refuse` raises before `Prompt` is sent | fires before the session append: `runners.py` 675-700 |
| `on_user_message_callback` | `Prompt` | `Block` raises pre-append, so the bytes never land in session history | `runners.py` 675-700 |
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: return a dict — ADK skips execution, and the dict becomes the function response the model reads. `Refuse` raises. | `functions.py` 611-641 |
| `after_tool_callback` | `ToolResult` | `ReplaceOutput{output}`: return a dict — it replaces the result the model sees. `Block` raises. | `functions.py` 652-683 |
| `on_tool_error_callback` | `ToolResult` with `Failure` outcome | return a dict to convert, or re-raise | `base_plugin.py` 348 |
| `before_tool_callback` on a sub-agent tool | `ToolCall{spawn:true}` | as `ToolCall`. The plugin holds the `AllowCall{spawn}` binding for the child scope | `functions.py` 611-641 |
| `before_agent_callback` (child) | `ChildStart` | `Refuse` raises | `base_plugin.py` 198 |
| `after_agent_callback` (child) | `TurnEnd` (child) | observe | `base_plugin.py` 217 |
| `after_tool_callback` on the sub-agent return | `SpawnResult` | `ChildReturn{value}` or `ReplaceOutput`: return a dict — the one point where the plugin can substitute the value the parent receives | `functions.py` 652-683 |
| `after_run_callback` | `TurnEnd` (root) | observe | `base_plugin.py` 174 |
| `on_run_error_callback` | `TurnEnd` (root, failure) | observe — notification-only, sufficient for an Ack-only event | `base_plugin.py` 394 |
| `on_agent_error_callback` (child) | `TurnEnd` (child, failure) | observe | `base_plugin.py` 374 |
| `before_run_callback`, `before_agent_callback` (root) | none | pass through — `Prompt` already gates the same bytes | — |
| `before_model_callback`, `after_model_callback`, `on_model_error_callback`, `on_event_callback` | none | liveness gates: raise when the `/hook` channel is down, pass otherwise | `base_plugin.py` 233, 253, 272, 155 |

`ChildEnd` is unfed by design. Return substitution is enforceable only where the parent receives the value, so returns cross at `SpawnResult`. The `appa-adapter-claude-code` parse map makes the same choice.

Sub-agent identity: kagent dispatches every declared remote agent as an ordinary ADK tool, `KAgentRemoteA2AToolset` ([types.py#L521](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/types.py#L521)). The plugin classifies a tool call as a spawn by the tool's type.

### Wire and codec

The plugin emits one JSON event per callback: event kind, trajectory ids, tool name, raw argument bytes, outcome, and value fields — the data the matching `HookEvent` variant needs. The `appa-adapter-kagent` Rust crate parses this wire into `HookEvent` and renders each `HookDecision` into the response the plugin enforces, through the `Codec` contract of `appa-runtime-api` (`parse`, `render` — `appa-runtime-api/src/lib.rs`). The wire carries no policy meaning. Raw tool arguments cross as spelled, and the Engine canonicalizes them.

### Trajectory identity

- Root `TrajectoryId`: the ADK session id with a harness prefix, per the `appa-runtime-api` convention.
- Child classification: the child Actor reads kagent's inbound call metadata. The executor lands that metadata in ADK session state ([_agent_executor.py#L212-L214](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L212-L214)). A delegated entry feeds `ChildStart`. A plain external entry feeds `SessionStart` and `Prompt`.
- Known limits: with `isolate_sessions`, ADK generates the child context id inside `run_async`, so the parent-side `ChildStart` correlation uses the spawn binding, not a pre-known child id. A bare-message A2A return carries no child session id. Both cases resolve at the shared runtime through the `SpawnResult` binding.

## Cross-actor children

A kagent parent and each delegated child run in separate Substrate Actors. The hooks of one trajectory therefore come from two plugin instances:

```text
parent Actor plugin:  ToolCall{spawn:true} ... SpawnResult
child Actor plugin:   ChildStart, Prompt-less child turns, TurnEnd (child)
```

Both must reach the same `appa-runtime`. A per-Actor runtime would split one trajectory into two logs, so the shared-service profile is the default. Verify Substrate egress from Actors to the runtime service in the target cluster before rollout. This is the one open operational item.

## Deployment profiles

### Shared runtime service (default)

One `appa-runtime` per cluster or namespace. `APPA_RUNTIME_URL` in `Harness.spec.env` names it. Only the runtime mounts policy, credentials, and the durable `appa.db` volume. Actors hold no APPA state. Required for any deployment with delegated children.

### Single-actor loopback

The adapter image starts `appa-runtime` as a second process and sets `APPA_RUNTIME_URL=http://127.0.0.1:8787`. The runtime listens on loopback only. Valid only for agents with no delegated children, because each Actor holds a separate trajectory log.

## Harness wiring and rollout

```yaml
kind: Harness
spec:
  kagent: {}
  workload:
    image: ghcr.io/archestra-ai/appa-adapter-kagent@sha256:<digest>
  env:
    - name: APPA_RUNTIME_URL
      value: http://appa-runtime.appa-system:8787
  substrate: { workerPoolRef: {name: <pool>}, snapshotPolicy: {location: <loc>} }
  allowedAgentTemplates:
    selector: { matchLabels: { <team labels> } }
```

- Pairing: the controller matches each `AgentTemplate` against every same-namespace Harness selector and reconciles one Actor per pair ([collections.go#L85-L102](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/controller/collections.go#L85-L102)).
- Rollout is "move the label match", never "add a second match". A template matched by two Harnesses runs twice — once gated, once not. Make old and new selectors disjoint.
- Env budget: the Substrate path caps an Actor at 32 total env vars ([actor_template.go#L50-L52](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L50-L52)). The compiler's own vars count against the cap. The adapter needs one.
- Prerequisites: helm `controller.substrate.enabled=true`, the `ate-system` install, and a `WorkerPool` — the stock requirements of the Substrate path ([values.yaml#L327-L334](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/helm/kagent/values.yaml#L327-L334)).

## Known gaps and handling

| Gap | Handling |
|---|---|
| No callback at ADK session creation | `SessionStart` synthesizes at first invocation, sent before `Prompt` from the same callback. A never-invoked session emits nothing and flows nothing. |
| A hard Actor crash emits no `TurnEnd` | `appa-runtime` recovery classifies the open dispatch as `Indeterminate` at the next admitted event. |
| CRD in-process sub-agents (`sub_agents`) unsupported by Python `AgentConfig` | The adapter refuses the config at startup instead of dropping children. CRD multi-agent topologies stay on `remote_agents`, which compile to gated tools. |
| Upstream has no plugin config knob | The entrypoint replays `static` behavior through public calls. CI pins `kagent-adk` and google-adk versions and runs an equivalence check on each bump. Propose a `KAGENT_PLUGINS`-style upstream knob to delete the duplication. |
| Claude and Codex harness variants, non-ADK wrappers, stock-image agents | Out of scope. The Claude harness omits hooks from its supported contract ([config.go#L48-L50](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/harness/claude/config/config.go#L48-L50)). |

## PR sequence

| PR | Change |
|---|---|
| 1 | `appa-adapter-kagent` Rust codec crate: wire parse to `HookEvent`, decision render, `Adapter` enum variant, unit tests against recorded wire fixtures |
| 2 | `appa-adapter-kagent` Python package: `AppaHookPlugin` with the callback table above, fail-closed transport, liveness gates |
| 3 | Entrypoint: materialize, strict config schema and refusal rules, stock plugin parity, `KAgentApp` assembly |
| 4 | OCI image build with pinned base digest, SBOM, and provenance, plus the single-actor loopback variant |
| 5 | End-to-end harness: kind cluster, Substrate path, shared runtime service, cross-actor child scenario |

No kagent PR and no Google ADK PR is required. The optional upstream contribution (a plugin config knob) is independent and non-blocking.

## Verification matrix

Adapter tests:

- Callback-to-event mapping for all rows of the table, including the spawn classification by tool type.
- Deny path: a `DenyCall` dict skips execution and reaches the model as the function response.
- Replace path: `ReplaceOutput` and `ChildReturn` substitution at `after_tool_callback`.
- Pre-append barrier: a blocked `Prompt` leaves no trace in session history.
- Fail closed: runtime down at each callback blocks the action, and liveness gates hold model and emission callbacks.
- Startup refusal: missing `APPA_RUNTIME_URL`, `sub_agents` present, unknown config fields.
- No link from the codec crate to `appa-runtime` or `appa-engine`, and no policy state in the plugin.

Equivalence tests:

- Entrypoint output matches stock `static` for the same rendered config, minus the added plugin: same agent shape, same stock plugins, same serving surface.
- Re-run on every `kagent-adk` and google-adk version bump.

End-to-end tests:

- Declarative agent on a kind cluster with the Substrate path: gated tool calls, replaced results, blocked prompts.
- Parent and delegated child in separate Actors against one shared runtime: one trajectory, `ChildStart` and `SpawnResult` correlated.
- Crash window: kill the Actor between `ToolCall` and `ToolResult`, then make sure the runtime reports the dispatch `Indeterminate`.
- Double-match rollout guard: the rollout check detects and reports a template matched by two Harnesses.
