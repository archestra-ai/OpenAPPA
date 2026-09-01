# kagent adapter implementation plan

`appa-adapter-kagent` is the Rust codec crate: a workspace crate compiled into `appa-runtime`, which selects it through its closed `Adapter` enum ([appa-runtime/src/main.rs](../../appa-runtime/src/main.rs)) as `appa_adapter_kagent::codec()`. The agent side wraps the kagent ADK runtimes and takes its names from them. The `appa-kagent-adk` python package (plugin + entrypoint) ships as the `ghcr.io/archestra-ai/appa-kagent-adk` image. The `appa-kagent-adk-go` Go module (plugin + runtime main) ships as the `ghcr.io/archestra-ai/appa-kagent-adk-go` image. The crate name never names an image. Both images read `APPA_RUNTIME_URL`, both emit the same adapter wire, and the one codec crate parses it.

## Source baselines

Stable release — the lane users install today:

- kagent [`v0.9.12`](https://github.com/kagent-dev/kagent/releases/tag/v0.9.12) (2026-07-20), the `kagent.dev/v1alpha2` API the public docs describe.
- kagent-adk 0.3.0. google-adk 1.31.1 — the lock resolution. The constraint is `google-adk>=1.28.1,<2` ([pyproject.toml#L25](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/pyproject.toml#L25)). The google_adk-1.31.1 wheel verifies the python-side callback claims below (wheel citations give paths and lines inside it).
- Go ADK: `google.golang.org/adk v1.4.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/go.mod#L50)) — the v1 line, before the v2 plugin API.

Release-candidate line — mid-cutover, in two observed states:

- Tag [`v0.10.0-rc4`](https://github.com/kagent-dev/kagent/releases/tag/v0.10.0-rc4) (`af84a618`, 2026-08-26): still the v1alpha2 `Agent` → Deployment controller, plus the `controller.goAgentImage` value. Go ADK: `google.golang.org/adk/v2 v2.1.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/go.mod#L50)). The workspace lock resolves google-adk 2.8.0.
- Main at [`52cc4de2`](https://github.com/kagent-dev/kagent/commit/52cc4de2a044a5062d10c4f189d863937c1bb0f9) (2026-09-01): the tree removes the v1alpha2 Agent controller, and agents are `v1alpha3` `AgentTemplate` × `Harness` pairs compiled to Substrate Actors. Go ADK: `google.golang.org/adk/v2 v2.2.0`. Python lock: google-adk 2.8.0 ([uv.lock#L1118-L1119](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/uv.lock#L1118-L1119)).

The `adk/v2` plugin surface is the same at both tags: `plugin/plugin.go` matches byte for byte between [v2.1.0](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go) and [v2.2.0](https://github.com/google/adk-go/blob/v2.2.0/plugin/plugin.go), and every callback signature matches.

OpenAPPA: `appa-runtime-api` hook vocabulary (`appa-runtime-api/src/lib.rs`) and the `appa-runtime` `/hook` endpoint (`appa-runtime/src/main.rs`).

## Target matrix

Five cells. Every diagram, mapping table, and end-to-end test in this plan names its cell.

| Cell | kagent | Runtime | ADK lock | Delivery knob | Image |
|---|---|---|---|---|---|
| A-py | v0.9.12 | python (CRD default) | google-adk 1.31.1 | helm `controller.agentImage` | `appa-kagent-adk` |
| B1-py | v0.10.0-rc4 | python (opt-in — the default flips to go) | google-adk 2.8.0 | helm `controller.agentImage` | `appa-kagent-adk` |
| B1-go | v0.10.0-rc4 | go (default) | adk/v2 v2.1.0 | helm `controller.goAgentImage` | `appa-kagent-adk-go` + `-full` |
| B2-py | main `52cc4de2` | python | google-adk 2.8.0 | `Harness.spec.workload.image` | `appa-kagent-adk` |
| B2-go | main `52cc4de2` | go | adk/v2 v2.2.0 | `Harness.spec.workload.image` | `appa-kagent-adk-go` |

Go-runtime agents on v0.9.12 are no cell: that tree has no Go-image knob and a v1 Go ADK without the plugin API. The gaps table names the workaround.

## Architecture decision

No kagent fork and no Google ADK fork, in either runtime:

- **Python runtime**: `KAgentApp(plugins=[...])` is a public constructor parameter of the published `kagent-adk` package ([v0.9.12 _a2a.py#L63](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L63)), forwarded into the ADK plugin manager. kagent registers its own plugins through it ([cli.py#L69-L79](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L69-L79)). The stock entrypoint keeps a closed plugin list, and no config adds to it, so the `appa-kagent-adk` image carries its own entrypoint that makes the same public calls and appends one plugin.
- **Go runtime**: The kagent Go runtime is the official Google Go ADK. On the release-candidate line it registers plugins through the ADK v2 plugin API — the kagent runner adapter itself passes `runner.PluginConfig{Plugins: ...}` ([v0.10.0-rc4 go/adk/pkg/runner/adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)) — but the build compiles the list in, with no config knob. `appa-kagent-adk-go` is therefore a replacement runtime main, built on the public kagent `go/adk` packages, that registers `AppaPluginKagent` (Go) in that list.
- Delivery is always an image reference the operator already controls. The lanes below name the knob per tree.

Both plugins hold no policy state. They serialize callback moments into wire events, send them to `APPA_RUNTIME_URL`, and enforce the answered `HookDecision`. `appa-runtime` owns policy, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db`.

## Delivery lanes

### Lane A — stable release (v1alpha2 `Agent` → Deployment)

The shipped controller reconciles every `Agent` into a plain Deployment + Service. The declarative runtime image is install configuration:

- **Python (the stable default runtime)**: `spec.declarative.runtime` defaults to `python` in the shipped CRD ([agent_types.go#L175](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L175)). Helm `controller.agentImage.{registry,repository,tag}` flows into the controller ConfigMap as `IMAGE_*` ([controller-configmap.yaml#L12-L18](https://github.com/kagent-dev/kagent/blob/v0.9.12/helm/kagent/templates/controller-configmap.yaml#L12-L18)) and becomes the agent Deployment image. Pointing it at `appa-kagent-adk` gates every python-runtime agent with zero agent changes.
- **Go**: v0.9.12 has no Go-image value and no Go-image controller flag, and its Go ADK is the v1 line without the v2 plugin API. Go-runtime agents are opt-in there. To gate one on stable, set `runtime: python` on that Agent (one field) — or move to lane B, which carries `appa-kagent-adk-go`.

Runtime contract the python image must keep — the controller sets container args, not the command:

- Accept `--host <bind> --port 8080 --filepath /config` ([deployments.go#L175-L179](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/deployments.go#L175-L179)).
- Read `config.json` and `agent-card.json` from the per-agent Secret mounted at `/config` ([manifest_builder.go#L243](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L243)).
- Serve A2A on port 8080 and answer readiness at `/.well-known/agent-card.json` ([manifest_builder.go#L532](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L532)).

```text
cell A-py — kagent v0.9.12 · python · google-adk 1.31.1

helm controller.agentImage = appa-kagent-adk
  │  rendered into the controller ConfigMap as
  │  IMAGE_REGISTRY / IMAGE_REPOSITORY / IMAGE_TAG
  ▼  one Deployment + Service per declarative Agent
┌─ agent pod · runtime: python (the CRD default) ───────┐
│  image  appa-kagent-adk                               │
│  args   --host <bind> --port 8080 --filepath /config  │
│  mount  Secret → /config: config.json, agent-card     │
│  ready  GET /.well-known/agent-card.json              │
│                                                       │
│  entrypoint.py                                        │
│    AgentConfig.model_validate — refuse unknown fields │
│    KAgentApp(plugins=[ ..stock.., AppaPluginKagent ]) │
│  kagent-adk 0.3.0 · google-adk 1.31.1 · 12 callbacks  │
└──────────────────────────┬────────────────────────────┘
                           ▼  POST $APPA_RUNTIME_URL/hook
```

Rollout: one `helm upgrade` re-renders every declarative python agent onto the OpenAPPA image, cluster-wide, with no double-run hazard. Staged rollout: `spec.declarative.deployment.imageRegistry` overrides the registry component per agent ([agent_types.go#L392](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L392)) — point pilot agents at a registry that serves the OpenAPPA image under the stock repository path. `APPA_RUNTIME_URL` arrives as a baked image default or per agent via `spec.declarative.deployment.env` ([agent_types.go#L443-L445](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L443-L445)).

### Lane B — release-candidate line

Two observed states, one adapter story:

**B1 — the current release candidates (v1alpha2 `Agent` → Deployment, both image knobs).** Same Deployment path as lane A, with three differences. The runtime default flips to `go` ([rc4 agent_types.go#L235-L241](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/api/v1alpha2/agent_types.go#L235-L241)). `controller.goAgentImage.{registry,repository,tag}` exists beside `controller.agentImage` ([rc4 controller-configmap.yaml#L28](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/helm/kagent/templates/controller-configmap.yaml#L28), [app.go#L226](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/pkg/app/app.go#L226)). And agents with skills or `executeCodeBlocks` resolve a `<tag>-full` Go-image variant, so `appa-kagent-adk-go` publishes both tags. Point both values at the matching images to gate every declarative agent — `appa-kagent-adk` gates the python agents, and `appa-kagent-adk-go` gates the go agents. Foundry-model agents require the Go runtime by compiler validation ([rc4 compiler.go#L224-L227](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/compiler.go#L224-L227)). `appa-kagent-adk-go` covers them, because it is a Go ADK runtime. Both runtimes receive the same args and keep the lane A readiness contract ([rc4 deployments.go#L176-L181](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/deployments.go#L176-L181), [manifest_builder.go#L569](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/manifest_builder.go#L569)).

```text
cell B1-py — kagent v0.10.0-rc4 · python · google-adk 2.8.0

helm controller.agentImage = appa-kagent-adk
  │  the same ConfigMap IMAGE_* path as cell A-py;
  │  the CRD default runtime is now go, so python
  ▼  agents carry runtime: python explicitly
┌─ agent pod · runtime: python ─────────────────────────┐
│  args, /config Secret, readiness — as cell A-py       │
│  entrypoint.py                                        │
│    KAgentApp(plugins=[ ..stock.., AppaPluginKagent ]) │
│  google-adk 2.8.0 · 14 callbacks                      │
└──────────────────────────┬────────────────────────────┘
                           ▼  POST $APPA_RUNTIME_URL/hook
```

```text
cell B1-go — kagent v0.10.0-rc4 · go · adk/v2 v2.1.0

helm controller.goAgentImage = appa-kagent-adk-go
  │  ConfigMap GO_IMAGE_REGISTRY / _REPOSITORY / _TAG;
  │  skills or executeCodeBlocks resolve <tag>-full;
  ▼  Foundry-model agents compile onto this runtime
┌─ agent pod · runtime: go (the rc default) ────────────┐
│  image  appa-kagent-adk-go — one static binary        │
│  args   --host <bind> --port 8080 --filepath /config  │
│  ready  GET /.well-known/agent-card.json              │
│                                                       │
│  main: rebuild the agent from the rendered config,    │
│    then runner.PluginConfig{Plugins:                  │
│      [ ..stock.., AppaPluginKagent ]}                 │
│  adk/v2 v2.1.0 · 12 callbacks · no error-turn cb      │
└──────────────────────────┬────────────────────────────┘
                           ▼  POST $APPA_RUNTIME_URL/hook
```

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

- The CRD requires `workload.image` and pins it by digest ([harness_types.go#L34-L40](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/api/v1alpha3/harness_types.go#L34-L40)). `spec.env` carries `APPA_RUNTIME_URL`, with Secret refs available.
- Pairing: the controller matches each `AgentTemplate` against every same-namespace Harness selector and reconciles one Actor per pair ([collections.go#L85-L102](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/controller/collections.go#L85-L102)). Rollout is "move the label match", never "add a second match" — a template matched by two Harnesses runs twice. Make old and new selectors disjoint.
- Config arrives as `KAGENT_CONFIG_JSON` / `KAGENT_AGENT_CARD_JSON` env ([actor_template.go#L43-L44](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L43-L44)). `materialize_from_env` in the python entrypoint handles both deliveries and is a no-op on the Deployment path. The Actor serves on port 8081 ([actor_template.go#L74](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L74)). The Substrate path caps an Actor at 32 total env vars ([actor_template.go#L50-L52](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L50-L52)), and the adapter needs one.
- Prerequisites: helm `controller.substrate.enabled=true`, the `ate-system` install, and a `WorkerPool` — the stock Substrate-path requirements.
- Templates whose compiled config carries in-process `sub_agents` ([kagent/compiler.go#L172](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/kagent/compiler.go#L172)) refuse at the python entrypoint (stock parsing drops them silently — the adapter must not). The python runtime has no in-process sub-agent field. Python multi-agent uses `remote_agents`, and the runtime adds them as tools, so the tool gate already covers them. This refusal therefore guards a runtime mismatch, not a python feature: a Go-compiled config with in-process children reaches the python image. Upstream rejects out-of-process (`Dedicated`) sub-agent tools ([compiler.go#L149-L151](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/compiler.go#L149-L151)). `appa-kagent-adk-go` consumes the Go-shaped config natively and carries these topologies once its mapping verifies.
- Re-verify this sub-lane against the first release that ships v1alpha3 before acting on it.

```text
cell B2-py — kagent main 52cc4de2 · python · google-adk 2.8.0

Harness.spec.workload.image = appa-kagent-adk@sha256:…
  │  allowedAgentTemplates selector ∧ same-namespace
  │  AgentTemplate ─▶ one Substrate Actor per pair
  ▼  (keep old and new selectors disjoint)
┌─ Substrate Actor · serves on port 8081 ───────────────┐
│  env  KAGENT_CONFIG_JSON · KAGENT_AGENT_CARD_JSON     │
│       APPA_RUNTIME_URL from Harness spec.env          │
│       cap: 32 env vars total, the adapter adds one    │
│                                                       │
│  entrypoint.py materialize_from_env → the A-py flow   │
│    refuses configs with compiled sub_agents           │
│  google-adk 2.8.0 · 14 callbacks                      │
└──────────────────────────┬────────────────────────────┘
                           ▼  POST $APPA_RUNTIME_URL/hook
```

```text
cell B2-go — kagent main 52cc4de2 · go · adk/v2 v2.2.0

Harness.spec.workload.image = appa-kagent-adk-go@sha256:…
  │  pairing and env delivery as cell B2-py
  ▼
┌─ Substrate Actor · serves on port 8081 ───────────────┐
│  go main reads KAGENT_CONFIG_JSON — the go-shaped     │
│    config, in-process sub_agents included — then      │
│    registers runner.PluginConfig{Plugins:             │
│      [ ..stock.., AppaPluginKagent ]}                 │
│  adk/v2 v2.2.0 · plugin set identical to v2.1.0       │
└──────────────────────────┬────────────────────────────┘
                           ▼  POST $APPA_RUNTIME_URL/hook
```

## Quickstart option

The quickstart is optional and orthogonal to everything above — same plugins, same wire, same codec, same runtime images. Skipping it changes nothing. It exists so one operator can gate one agent in minutes, with no separate `appa-runtime` deployment.

`appa-kagent-quickstart` is one image that bundles both runtime layers and `appa-runtime` itself. The entrypoint starts `appa-runtime` on `127.0.0.1:8787` with a packaged example policy, points `APPA_RUNTIME_URL` at it, and execs the runtime that matches the rendered config. The pod keeps the stock args, port, and readiness contract, and the bundled runtime serves `/mcp` too, so remedy plans execute the same way. Point `controller.agentImage` at it on v0.9.12 (python agents), or both image values on the v0.10 release candidates.

```text
quickstart — one pod, nothing else to deploy

helm controller.agentImage = appa-kagent-quickstart
     (v0.10: also controller.goAgentImage)
        ▼
┌─ agent pod ───────────────────────────────────────────┐
│  entrypoint: start appa-runtime on 127.0.0.1:8787,    │
│  then exec the runtime the rendered config matches    │
│                                                       │
│  kagent runtime (python or go) + AppaPluginKagent     │
│    │  POST http://127.0.0.1:8787/hook                 │
│    ▼                                                  │
│  appa-runtime · policy · Engine · appa.db — pod-local │
└───────────────────────────────────────────────────────┘
```

Quickstart limits:

- Trajectory state and `appa.db` live in the pod and die with it.
- A parent and a called agent run as two pods with two bundled runtimes, so their hooks land in two trajectories. Cross-workload correlation needs one `appa-runtime` that both reach.
- One packaged example policy per image build. Real policy work moves to a deployed `appa-runtime`.

## Runtime adapters

### Python — `AppaPluginKagent` on google-adk (verified)

The plugin implements google-adk `BasePlugin`. Each gated callback sends one wire event to `POST $APPA_RUNTIME_URL/hook` and enforces the decision that comes back. A transport failure or a non-contract response raises, and ADK wraps the exception and aborts the invocation (`plugin_manager.py`: 288-305 in 1.31.1, 316-322 in 2.8.0). One mapping table per locked ADK version follows. Wheel citations name paths and lines inside the wheel for that version.

#### google-adk 1.31.1 — cell A-py

The 1.31.1 wheel defines 12 lifecycle callbacks (`base_plugin.py` lines 114, 136, 155, 174, 198, 217, 233, 253, 272, 297, 321, 348).

| ADK callback | HookEvent | Enforcement at the callback | 1.31.1 wheel evidence |
|---|---|---|---|
| `on_user_message_callback`, first invocation of a fresh session | `SessionStart` | `Refuse` raises before `Prompt` is sent | `runners.py` 1537-1541 |
| `on_user_message_callback` | `Prompt` | `Block` raises pre-append, so the bytes never land in session history | fires before the append: `runners.py` 1537 then 1550-1556 |
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: return a dict — ADK skips execution, and the dict becomes the function response the model reads. `Refuse` raises. | `functions.py` 509-534, 588-592 |
| `before_tool_callback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return None — the call passes through to `/mcp` on the runtime, which spends the vouch | `functions.py` 509-534 |
| `after_tool_callback` | `ToolResult` | `ReplaceOutput{output}`: return a dict — it replaces the result the model sees. `Block` raises. The plugin recognizes its own deny dicts here (a denied call flows through this callback too) and reports them once. | `functions.py` 547-576 |
| `on_tool_error_callback` | `ToolResult` with `Failure` outcome | return a dict to convert, or re-raise. Does not fire for a `before_tool_callback` raise — a `Refuse` stays terminal. | `functions.py` 535-545 |
| `before_tool_callback` on an agent tool | `ToolCall{spawn:true}` | as `ToolCall`. The plugin holds the `AllowCall{spawn}` binding for the child scope | `functions.py` 509-534 |
| `before_agent_callback` (local sub-agent) | `ChildStart` | a returned `Content` ends the child before its body runs | `base_agent.py` 288-296, 447-452 |
| `after_tool_callback` on the agent-tool return | `SpawnResult` | `ChildReturn{value}` or `ReplaceOutput`: return a dict — the one point where the plugin can substitute the value the parent receives | `functions.py` 547-576 |
| `after_run_callback` | `TurnEnd` (root) | observe — also fires after a `before_run` halt | `runners.py` 843-861, 952 |
| `before_run_callback` | none | pass through — `Prompt` already gates the same bytes | `runners.py` 843-861 |
| `before_model_callback`, `after_model_callback`, `on_model_error_callback`, `on_event_callback` | none | liveness gates: raise when the `/hook` channel is down, pass otherwise | `base_plugin.py` 233, 253, 272, 155 |

By design, nothing feeds `ChildEnd`. Return substitution is enforceable only where the parent receives the value, so returns cross at `SpawnResult`.

1.31.1 notes: no error-turn callback exists — `on_run_error_callback` and `on_agent_error_callback` are absent from the wheel — and ADK skips `after_run_callback` when a run dies on an unhandled error (`runners.py` 949-954, no `finally`). The model-error and tool-error callbacks catch the common failures earlier, and `appa-runtime` recovery classifies the rest `Indeterminate` at the next admitted event. An `AgentTool` child runs under a fresh child Runner that inherits the plugin list from the parent, so coverage continues there.

#### google-adk 2.8.0 — cells B1-py and B2-py

The 2.8.0 wheel defines 14 lifecycle callbacks: the twelve above at the same `base_plugin.py` lines, plus `on_agent_error_callback` (374) and `on_run_error_callback` (394). The shared rows keep the 1.31.1 semantics, re-verified at the 2.8.0 sites:

| ADK callback | HookEvent | Enforcement at the callback | 2.8.0 wheel evidence |
|---|---|---|---|
| `on_user_message_callback`, first invocation of a fresh session | `SessionStart` | `Refuse` raises before `Prompt` is sent | `runners.py` 677 |
| `on_user_message_callback` | `Prompt` | `Block` raises pre-append: the callback runs at 677, the session append at 705-708 | `runners.py` 677, 705-708 |
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: the returned dict skips execution and becomes the function response the model reads. `Refuse` raises. | `functions.py` 611-622 |
| `before_tool_callback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return None — the call passes through to `/mcp` on the runtime, which spends the vouch | `functions.py` 611-622 |
| `after_tool_callback` | `ToolResult` | `ReplaceOutput{output}`: the returned dict replaces the result. Deny dicts are self-recognized and reported once. | `functions.py` 652-656 |
| `on_tool_error_callback` | `ToolResult` with `Failure` outcome | return a dict to convert, or re-raise | `functions.py` 544-563, 595, 641 |
| `before_tool_callback` on an agent tool | `ToolCall{spawn:true}` | as `ToolCall`, holding the `AllowCall{spawn}` binding | `functions.py` 611-622 |
| `before_agent_callback` (sub-agent) | `ChildStart` | a returned `Content` ends the child before its body runs. Sub-agents re-enter `run_async`, so this fires per child. | `base_agent.py` 320, 382 |
| `after_tool_callback` on the agent-tool return | `SpawnResult` | `ChildReturn{value}` or `ReplaceOutput` substitution | `functions.py` 652-656 |
| `on_agent_error_callback` | `TurnEnd` (child, `Failure`) | observe — the error still propagates | `base_agent.py` 632 |
| `on_run_error_callback` | `TurnEnd` (root, `Failure`) | observe — ADK treats the callback as notification and suppresses anything it raises, so this point cannot hold | `runners.py` 96-108, 786-790 |
| `after_run_callback` | `TurnEnd` (root) | observe — also fires after a `before_run` halt | `runners.py` 791 |
| `before_run_callback` | none | pass through — `Prompt` already gates the same bytes | `base_plugin.py` 136 |
| `before_model_callback`, `after_model_callback`, `on_model_error_callback`, `on_event_callback` | none | liveness gates: raise when the `/hook` channel is down, pass otherwise | `base_plugin.py` 233, 253, 272, 155 |

The two error rows close the error-turn gap on the lane B python cells. The gap stays open on the go cells, because adk/v2 has no error-turn callback (the go table below).

Entrypoint (python image), in order:

1. Parse the mounted or env-delivered config with a strict schema, and refuse unknown fields with an unready exit.
2. Validate with `AgentConfig.model_validate` and build the factory over `to_agent`.
3. Rebuild the stock plugin list with the stock conditions (STS token propagation, LLM passthrough), then append `AppaPluginKagent(APPA_RUNTIME_URL)`.
4. Append the reserved-tool toolset: a `McpToolset` over streamable HTTP at `$APPA_RUNTIME_URL/mcp` (see [Remedy-plan execution](#remedy-plan-execution)).
5. Construct `KAgentApp(...)`, call `.build()`, and serve on the controller-given host and port — the same calls as [cli.py#L88-L101](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L88-L101).

### Go — `AppaPluginKagent` on the Go ADK (design, verification pending)

`appa-kagent-adk-go` is a small runtime main that imports the public kagent `go/adk` packages, constructs the same agent the stock Go runtime builds from the rendered config, and registers the Go `AppaPluginKagent` through the ADK v2 plugin API — the registration point kagent itself uses ([rc4 adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)). It emits the same adapter wire as the python plugin, and it appends the reserved-tool toolset at construction.

The build composes upstream, and does not fork it. `appa-kagent-adk-go` is a separate Go module with one runtime main. Its `go.mod` requires the `github.com/kagent-dev/kagent/go` module, which exports the `go/adk` packages, and `google.golang.org/adk/v2` — pinned, fetched unmodified from the module proxy, locked by `go.sum`. `go build` links `AppaPluginKagent` into one static binary, and the image ships that binary under the stock args, port, and readiness contract. Go compiles the plugin list in, so the Go image adds its plugin at build time, where the python image adds its plugin at container start. The mapping verification also confirms the main uses only exported construction calls — the module imports no kagent `internal/` package.

#### adk/v2 v2.1.0 and v2.2.0 — cells B1-go and B2-go (design)

The plugin surface is the same at both tags, so one table serves both cells. A `plugin.Plugin` exposes 12 callbacks through its accessors ([plugin/plugin.go#L113-L158](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L113-L158)). No run-error and no agent-error callback exists, so the error-turn gap of google-adk 1.31.1 applies to the go cells too. Signature references are v2.1.0 — v2.2.0 shifts the `llmagent.go` lines by two and changes nothing in the set.

| Go callback | HookEvent | Behavior to verify before the image ships | Signature |
|---|---|---|---|
| `OnUserMessageCallback` | `SessionStart`, then `Prompt` | fires before the session append, and a returned error aborts the run | [plugin.go#L161](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L161) |
| `BeforeToolCallback` | `ToolCall{spawn}` | a non-nil map skips execution and reaches the model as the function response | [llmagent.go#L390](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L390) |
| `BeforeToolCallback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return a nil map — the call passes through to `/mcp` on the runtime | [llmagent.go#L390](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L390) |
| `AfterToolCallback` | `ToolResult` — `SpawnResult` on an agent tool | a non-nil map replaces the result the model sees | [llmagent.go#L399](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L399) |
| `OnToolErrorCallback` | `ToolResult` with `Failure` outcome | a map converts the error, and a returned error stays terminal | [llmagent.go#L405](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L405) |
| `BeforeAgentCallback` | `ChildStart` | a returned `Content` ends the child before its body runs | [agent.go#L129](https://github.com/google/adk-go/blob/v2.1.0/agent/agent.go#L129) |
| `AfterAgentCallback` | `TurnEnd` (in-process child) | fires once per sub-agent scope | [agent.go#L137](https://github.com/google/adk-go/blob/v2.1.0/agent/agent.go#L137) |
| `AfterRunCallback` | `TurnEnd` (root) | nothing — the signature returns no value, observation only | [plugin.go#L165](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L165) |
| `BeforeRunCallback` | none | liveness gate — `Prompt` already gates the same bytes | [plugin.go#L163](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L163) |
| `BeforeModelCallback`, `AfterModelCallback`, `OnModelErrorCallback`, `OnEventCallback` | none | liveness gates | [llmagent.go#L366-L378](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L366-L378), [plugin.go#L167](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L167) |

Verification status: the python tables are proven against their pinned wheels. The go table is design — its behavior column is the proof obligation against the locked `adk/v2` before the Go image ships. The Go runtime also serves Foundry-model agents, which the compiler ties to the Go runtime. Deliverables: both tags (`<tag>` and `<tag>-full`, the variant kagent resolves for agents with skills or `executeCodeBlocks`).

## Remedy-plan execution

A blocked call answers with feedback that quotes an offer id. The offered plan executes through `execute_remedy_plan` — the reserved MCP tool of the engine, runtime-provided and identical for every harness, served at `$APPA_RUNTIME_URL/mcp` from process start ([appa-runtime/src/mcp.rs](../../appa-runtime/src/mcp.rs)). The runtime refuses a call no hook vouched for, and executing the act spends the vouch.

Delivery is one more construction delta in each entrypoint. The python entrypoint appends a `McpToolset` over streamable HTTP at `$APPA_RUNTIME_URL/mcp` — the same classes kagent-adk itself uses for CRD MCP tools ([v0.9.12 types.py#L223](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L223), [rc4 types.py#L224](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/python/packages/kagent-adk/src/kagent/adk/types.py#L224)). The go main appends the toolset from [`tool/mcptoolset`](https://github.com/google/adk-go/tree/v2.1.0/tool/mcptoolset). `AppaPluginKagent` answers the `ToolCall` hook of the reserved tool with `PassControl` and lets the call pass — the dedicated row in each mapping table above.

Coverage per plan element ([appa-engine/src/plan.rs](../../appa-engine/src/plan.rs)):

| Plan element | On kagent |
|---|---|
| `Authorize(authority)` | Executes engine-side during the offer. Human review: below. |
| `Accept(narrowing)` | Executes engine-side. The narrowed call redispatches through the normal gate. |
| `Sanitize(sanitizer)` | Executes engine-side, and the sanitized result returns through the mapped after-tool path. |
| `Derive(sanitizer)` | Executes engine-side — the progress hop. |
| `Redispatch` | Needs no id and no reserved tool. The agent calls the named tool, and the normal `ToolCall` gate applies. |
| fork advice | Advice, never a remedy. The spawn gate is already mapped. |

### Human review

The Claude Code channel does not transplant. `appa-runtime` consults its human authority through MCP elicitation inside the still-open `execute_remedy_plan` call ([appa-runtime/src/elicit.rs](../../appa-runtime/src/elicit.rs)), and the MCP client in a kagent pod has no person on it. google-adk 1.31.1 has no elicitation support. 2.8.0 takes an `elicitation_callback` a pod could only answer programmatically — the replacement elicit.rs warns against. `tool/mcptoolset` shows no elicitation surface at either adk/v2 tag.

kagent carries its own human channel, wired end to end on both release lines. `requireApproval` in the Agent CRD flows into the compiled config ([v0.9.12 adk_api_translator.go#L1053-L1066](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/adk_api_translator.go#L1053-L1066), [rc4 #L1185-L1196](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/adk_api_translator.go#L1185-L1196)). ADK supports `require_confirmation` on tools — `McpTool` included — at both pinned python versions (1.31.1 `mcp_tool.py` 136 and 291, 2.8.0 `mcp_tool.py` 186). The A2A executor suspends the run, surfaces the request to the caller, and resumes with the decision ([v0.9.12 _agent_executor.py#L348-L421](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L348-L421), [rc4 #L349](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L349)). The caller is the kagent UI, the CLI, or an upstream A2A client. kagent also strips the confirmation parts before they reach the model ([v0.9.12 types.py#L16](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L16), [rc4 types.py#L541-L547](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/python/packages/kagent-adk/src/kagent/adk/types.py#L541-L547)) — the same isolation the elicitation channel keeps.

The go runtime carries the channel too. adk/v2 defines the confirmation contract at both tags — `ErrConfirmationRequired` and a `ConfirmationProvider` ([tool.go#L31-L35](https://github.com/google/adk-go/blob/v2.1.0/tool/tool.go#L31-L35), [#L119](https://github.com/google/adk-go/blob/v2.1.0/tool/tool.go#L119)) — and `mcptoolset` takes `RequireConfirmation` and `RequireConfirmationProvider` in its config ([set.go#L126-L131](https://github.com/google/adk-go/blob/v2.1.0/tool/mcptoolset/set.go#L126-L131)). The rc4 kagent go runtime bridges the confirmation over A2A ([hitl.go](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/a2a/hitl.go)) and strips the synthetic parts from the model ([approval.go](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/agent/approval.go)).

Design: gate `execute_remedy_plan` with `require_confirmation` — the python `Callable` form, or `RequireConfirmationProvider` on the go toolset — scoped to offers whose plan needs a person. The approval then rides the stock kagent suspend/resume flow. One decision stays open before this ships: kagent approves before execution, where the elicitation consult answers during it. Whether the stock approval stands as the proof the authority requires — or a kagent-shaped authority channel gets its own name — is a question the spec settles, not this plan. Until then, a plan that names a human review stands unexecuted: no answer grants nothing, and the offer stands.

## Annotators

A `[[tool]]` entry either declares the complete contract or names a registered annotator, which answers it per proposed call. The consult runs engine-side inside the `ToolCall` round-trip, and the envelope carries only the annotator declaration and the artifact ([appa-runtime/src/consult.rs](../../appa-runtime/src/consult.rs)). The kagent wire already supplies the artifact ingredients — the tool name and the raw argument bytes — so annotators need no kagent surface, no plugin change, and no wire change. A no-answer renders as `Refuse` on the `ToolCall` hook, never as model-facing feedback ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs)) — the mapped `Refuse` leg in each table above.

Three kagent-specific notes:

- **Timeout budget.** The plugin `/hook` client timeout must exceed the runtime consult budget, or a slow annotator — endpoint, command, or model builtin — becomes a spurious fail-closed block. ADK has no callback timeout, so kagent carries no fail-open hazard: a slow consult costs latency, bounded upstream by A2A client patience.
- **Tool naming and coverage.** An annotation pins to the canonical digest under the tool name as ADK dispatches it. `[[tool]]` entries and mandates must match that spelling for every toolset — an equivalence-test item below. The wildcard entry (`name = "*"`) is the recommended first posture for a fleet: CRD-declared toolsets produce a long tail the policy never names up front. Optional tooling: generate `[[tool]]` skeletons from the `Agent` and `RemoteMCPServer` resources in the cluster. The reserved `execute_remedy_plan` needs no entry — the runtime recognizes its own tool first.
- **Builtin provisioning.** Model-builtin annotators execute in the `appa-runtime` deployment ([appa-runtime/src/external.rs](../../appa-runtime/src/external.rs)). `builtin = "llm"` needs `[externals.llm]` and model egress from the runtime pod. `builtin = "claude-code"` needs the claude CLI where the runtime runs. The quickstart inherits the same needs, because it bundles the runtime.

## Wire and codec

Each plugin emits one JSON event per callback: event kind, trajectory ids, tool name, raw argument bytes, outcome, and value fields — the data the matching `HookEvent` variant needs. The `appa-adapter-kagent` Rust crate parses this wire into `HookEvent` and renders each `HookDecision` into the response the plugins enforce, through the `Codec` contract of `appa-runtime-api` (`parse`, `render`). The wire carries no policy meaning. Raw tool arguments cross as spelled, and the Engine canonicalizes them. The reserved tool crosses as spelled too — `execute_remedy_plan` — so the runtime recognizes its own tool and binds the vouch. One wire and one codec serve both runtime images.

### Labels and flow completeness

The contract triple — `delta`, `requires`, `emits` — is engine algebra, and no label crosses the wire in either direction. The engine narrows the trajectory label with `delta` when it admits a result. It checks `requires` — membership, `history`, and `attention` marks — against trajectory state at dispatch ([appa-engine/src/check.rs](../../appa-engine/src/check.rs)). It records `emits` into the effect ledger, and effects commit on `Success`, never on `Indeterminate`.

That algebra is sound only over the flows the runtime saw, so the plugin keeps one invariant: every value that enters model attention or leaves the agent crosses a mapped hook. On kagent the list is closed:

- User input crosses at `Prompt`, before the session append.
- Tool and child returns cross at `ToolResult` and `SpawnResult`.
- Delegated entries cross at `ChildStart`.
- ADK memory and artifact loaders are ordinary tools, so they cross the tool gate.
- The CRD-compiled instruction is static config, not a flow.

The liveness gates hold everything else when the `/hook` channel is down. The implemented model gates sinks at tool dispatch and defines no emission event — `TurnEnd` gates nothing by design ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs)). If the spec later defines a gated response sink, `on_event_callback` is the ready carrier on kagent: it can replace events. That is a forward path, not current behavior.

## Trajectory identity

- Root `TrajectoryId`: the ADK session id with a harness prefix, per the `appa-runtime-api` convention.
- Child classification: the plugin in the child pod reads the inbound kagent call metadata to recognize a delegated entry (on the v1alpha3 lane the executor lands it in session state — [_agent_executor.py#L212-L214](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L212-L214)). A delegated entry feeds `ChildStart`. A plain external entry feeds `SessionStart` and `Prompt`.
- A successful child reply arrives as `{"result": ...}` for Task replies and as a bare string for direct Message replies — the plugins handle both shapes.
- Parent and child run as separate workloads (Deployments on lane A/B1, Substrate Actors on B2), so the hooks of one trajectory come from two plugin instances. Both must reach the same `appa-runtime` — a per-pod runtime would split one trajectory into two logs.

## Known gaps and handling

| Gap | Lane / runtime | Handling |
|---|---|---|
| No callback at ADK session creation | all | `SessionStart` synthesizes at first invocation, sent before `Prompt`. A never-invoked session emits nothing and flows nothing. |
| No error-turn callback in google-adk 1.31.1 | A-py | Earlier error callbacks plus `Indeterminate` classification at recovery. Closed on the lane B python cells by the google-adk 2.8.0 error callbacks. |
| No error-turn callback in adk/v2 | B1-go, B2-go | v2.1.0 and v2.2.0 define no run-error and no agent-error callback. Recovery classifies the open dispatch `Indeterminate`, as on cell A-py. |
| Human review on remedy plans | all | The elicitation channel needs an interactive MCP client the pod lacks. The designed route is `require_confirmation` on the reserved tool over the stock kagent approval flow, present on both runtimes — its standing as the proof the authority requires is an open decision for the spec. Until then such plans stand unexecuted, and no answer grants nothing. |
| Go-runtime agents on stable | A / go | No Go-image knob and a v1 Go ADK in v0.9.12: flip those agents to `runtime: python` (one field), or adopt lane B. |
| Go mapping unproven | B / go | Verify against the locked `adk/v2` before shipping the Go image. Until then the Go lane is design, not a claim. |
| CRD in-process `sub_agents` on the python runtime | B2 / python | The entrypoint refuses the config instead of dropping children. `appa-kagent-adk-go` consumes them natively once verified. |
| BYO agents | all | Per-agent images outside any shared runtime image. Their authors add the one plugin line, in either language. |
| Sandbox kinds (`AgentHarness`, `SandboxAgent`) | all | Different subsystem, out of scope. |
| Upstream has no plugin config knob | all | The entrypoints replay stock behavior through public calls. CI pins kagent and ADK versions per lane and re-runs the equivalence checks on each bump. Propose an upstream plugin-loading knob to delete the duplication. |

## PR sequence

| PR | Change |
|---|---|
| 1 | `appa-adapter-kagent` Rust codec crate: wire parse to `HookEvent`, decision render, `Adapter` enum variant, unit tests against recorded wire fixtures |
| 2 | `appa-kagent-adk` Python package: `AppaPluginKagent` with the callback table, per-ADK deltas, fail-closed transport, liveness gates, deny-dict self-recognition, `PassControl` pass-through |
| 3 | Python entrypoint: strict config schema and refusal rules, stock plugin parity, both config deliveries, the controller args contract, the reserved-tool toolset |
| 4 | Python OCI image with pinned base digest, SBOM, and provenance |
| 5 | Lane A end-to-end: kind cluster with the stable chart, `controller.agentImage` swap, parent-and-child scenario against one shared runtime |
| 6 | `appa-kagent-adk-go`: adk/v2 mapping verification, the Go plugin and runtime main, both image tags, the reserved-tool toolset |
| 7 | Lane B end-to-end: the B1 dual-knob swap on the release-candidate chart, and B2 Harness × AgentTemplate on the Substrate path |
| 8 | Optional: `appa-kagent-quickstart` bundled image — both runtime layers, packaged `appa-runtime`, example policy, the quickstart entrypoint |

The plan requires no kagent PR and no Google ADK PR. The optional upstream contribution (a plugin config knob) is independent and non-blocking.

## Verification matrix

Adapter tests (per runtime):

- Callback-to-event mapping for every table row, including spawn classification by tool type and both child-return shapes.
- Deny path: a `DenyCall` skips execution, reaches the model as the function response, and is not double-reported.
- Replace path: `ReplaceOutput` and `ChildReturn` substitution at the after-tool point.
- Pre-append barrier: a blocked `Prompt` leaves no trace in session history.
- Fail closed: runtime down at each callback blocks the action, and liveness gates hold model and emission callbacks.
- Pass through: the reserved `execute_remedy_plan` call proceeds untouched on `PassControl`, and the runtime refuses an unvouched `/mcp` call.
- Startup refusal: missing `APPA_RUNTIME_URL`, unknown config fields, `sub_agents` on the python runtime.
- Args contract: the entrypoints accept the controller args and answer readiness at the stock endpoint.
- No link from the codec crate to `appa-runtime` or `appa-engine`, and no policy state in either plugin.

Equivalence tests:

- Each entrypoint output matches the stock counterpart for the same rendered config, minus the added plugin.
- Record the tool names each toolset dispatches, per ADK version. `[[tool]]` entries and mandates match that spelling.
- Re-run on every kagent and ADK version bump, per lane. The callback tables re-verify against the newly locked ADK.

End-to-end tests:

- Lane A: declarative python agent on a kind cluster with the stable chart and the `controller.agentImage` swap — gated tool calls, replaced results, blocked prompts.
- Lane B1: both image knobs swapped on the release-candidate chart — a python agent and a go agent gated side by side.
- Lane B2: `AgentTemplate` × `Harness` on the Substrate path — admission by selector, `KAGENT_CONFIG_JSON` delivery, the env-var cap respected.
- Cross-workload trajectory: parent and delegated child against one shared runtime — one trajectory, `ChildStart` and `SpawnResult` correlated.
- Remedy execution per plan element: accept-narrowing, authorize with a stock authority, sanitize, derive hop, and redispatch — each on a gated agent, with the vouch spent once per act.
- Human review: `require_confirmation` suspends the reserved call, an approval from the A2A client executes it, and a decline leaves the offer standing.
- Annotated tool: the consult happens once, the annotation pins to the canonical digest, and replay re-reaches the decision without a second consult.
- Annotator down: the gated call refuses at the `ToolCall` hook, and nothing model-facing crosses.
- Wildcard: a tool the policy never names routes through the wildcard annotator and runs annotated.
- Crash window: kill the agent workload between `ToolCall` and `ToolResult`, then make sure the runtime reports the dispatch `Indeterminate`.
- Error-turn window per cell: on cell A-py and both go cells, force an unhandled model failure and make sure recovery closes the turn at the next admitted event. On the lane B python cells, make sure the error callbacks feed the failure `TurnEnd`s.
