# kagent adapter implementation plan

`appa-adapter-kagent` is the Rust codec crate: a workspace crate compiled into `appa-runtime`, which selects it through its closed `Adapter` enum ([appa-runtime/src/main.rs](../../appa-runtime/src/main.rs)) as `appa_adapter_kagent::codec()`. The agent side wraps the kagent ADK runtimes and takes its names from them. The `appa-kagent-adk` python package (plugin + entrypoint) ships as the `ghcr.io/archestra-ai/appa-kagent-adk` image. The `appa-kagent-adk-go` Go module (plugin + runtime main) ships as the `ghcr.io/archestra-ai/appa-kagent-adk-go` image. The crate name never names an image. Both images read `APPA_RUNTIME_URL`, both emit the same adapter wire, and the one codec crate parses it.

## Source baselines

Stable release — the installed lane:

- kagent [`v0.9.12`](https://github.com/kagent-dev/kagent/releases/tag/v0.9.12) (2026-07-20), the `kagent.dev/v1alpha2` API the public docs describe.
- kagent-adk 0.3.0. google-adk 1.31.1 — the lock resolution. The constraint is `google-adk>=1.28.1,<2` ([pyproject.toml#L25](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/pyproject.toml#L25)). The google_adk-1.31.1 wheel verifies the python-side callback claims below (wheel citations give paths and lines inside it).
- Go ADK: `google.golang.org/adk v1.4.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/go.mod#L50)) — the v1 line, before the v2 plugin API.

Release-candidate line — mid-cutover, in two observed states:

- Tag [`v0.10.0-rc4`](https://github.com/kagent-dev/kagent/releases/tag/v0.10.0-rc4) (`af84a618`, 2026-08-26): still the v1alpha2 `Agent` → Deployment controller, plus the `controller.goAgentImage` value. Go ADK: `google.golang.org/adk/v2 v2.1.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/go.mod#L50)). The workspace lock resolves google-adk 2.8.0.
- Main at [`52cc4de2`](https://github.com/kagent-dev/kagent/commit/52cc4de2a044a5062d10c4f189d863937c1bb0f9) (2026-09-01): the tree removes the v1alpha2 Agent controller, and agents are `v1alpha3` `AgentTemplate` × `Harness` pairs compiled to Substrate Actors. Go ADK: `google.golang.org/adk/v2 v2.2.0`. Python lock: google-adk 2.8.0 ([uv.lock#L1118-L1119](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/uv.lock#L1118-L1119)).

The `adk/v2` plugin surface is the same at both tags: `plugin/plugin.go` matches byte for byte between [v2.1.0](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go) and [v2.2.0](https://github.com/google/adk-go/blob/v2.2.0/plugin/plugin.go), and every callback signature matches.

OpenAPPA: `appa-runtime-api` hook vocabulary (`appa-runtime-api/src/lib.rs`) and the `appa-runtime` `/hook` endpoint (`appa-runtime/src/main.rs`).

## Target matrix

Six cells. Every diagram, mapping table, and end-to-end test in this plan names its cell.

| Cell | kagent | Runtime | ADK lock | Delivery knob | Image |
|---|---|---|---|---|---|
| A-py | v0.9.12 | python (CRD default) | google-adk 1.31.1 | helm `controller.agentImage` | `appa-kagent-adk` |
| A-go | v0.9.12 | go (`spec.declarative.runtime: go`) | adk/v2 v2.1.0, inside the image | the name kagent derives from `controller.agentImage` | `appa-kagent-adk-go` |
| B1-py | v0.10.0-rc4 | python (opt-in — the default flips to go) | google-adk 2.8.0 | helm `controller.agentImage` | `appa-kagent-adk` |
| B1-go | v0.10.0-rc4 | go (default) | adk/v2 v2.1.0 | helm `controller.goAgentImage` | `appa-kagent-adk-go` + `-full` |
| B2-py | main `52cc4de2` | python | google-adk 2.8.0 | `Harness.spec.workload.image` | `appa-kagent-adk` |
| B2-go | main `52cc4de2` | go | adk/v2 v2.2.0 | `Harness.spec.workload.image` | `appa-kagent-adk-go` |

v0.9.12 has no Go-image knob: for an agent with `runtime: go` the controller derives the image name from `controller.agentImage` by replacing the last repository path segment with `golang-adk` and keeping the registry and tag. Cell A-go serves `appa-kagent-adk-go` under that derived name. The image carries its own adk/v2 and the kagent `go/adk` packages, so the v1 Go ADK in that tree plays no part; the v0.9.12 controller's session and task API is what the image talks to, and the demo chart runs the cell against it.

## Architecture decision

Each runtime takes the plugin through a public surface it already ships:

- **Python runtime**: `KAgentApp(plugins=[...])` is a public constructor parameter of the published `kagent-adk` package ([v0.9.12 _a2a.py#L63](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L63)), forwarded into the ADK plugin manager. kagent registers its own plugins through it ([cli.py#L69-L79](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L69-L79)). The stock entrypoint keeps a closed plugin list, and no config adds to it. So the `appa-kagent-adk` image carries its own entrypoint. That entrypoint makes the same public calls and appends one plugin.
- **Go runtime**: The kagent Go runtime is the official Google Go ADK. On the release-candidate line it registers plugins through the ADK v2 plugin API. The kagent runner adapter itself passes `runner.PluginConfig{Plugins: ...}` ([v0.10.0-rc4 go/adk/pkg/runner/adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)). But the build compiles the list in, with no config knob. `appa-kagent-adk-go` is therefore a replacement runtime main, built on the public kagent `go/adk` packages, that registers `AppaPluginKagent` (Go) in that list.
- Delivery is always an image reference the operator already controls. The lanes below name the knob per tree.

Both plugins hold no policy state. They serialize callback moments into wire events, send them to `APPA_RUNTIME_URL`, and enforce the answered `HookDecision`. Beyond their transport and the per-session id pin, the one thing either plugin keeps between callbacks is the `review` text a `deny_call` handed it, until the reviewed offer's confirmation returns ([Human review](#human-review)). Policy, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db` live in `appa-runtime`.

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
  │  the CRD default runtime is go on this tree, so
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

`appa-kagent-quickstart` is one image that bundles both runtime layers and `appa-runtime` itself. The entrypoint starts `appa-runtime` on `127.0.0.1:8787` with a packaged example policy, points `APPA_RUNTIME_URL` at it, and execs the runtime that matches the rendered config. A pod that arrives with `APPA_RUNTIME_URL` already set keeps the bundled runtime off and execs the gated runtime against that shared `appa-runtime`. The demo uses the image both ways: as kagent's `controller.agentImage` for the fleet (set on the kagent install, the chart's prerequisite), and as the shared runtime pod's container, run as `appa runtime`. The pod keeps the stock args, port, and readiness contract, and the bundled runtime serves `/mcp` too, so remedy plans execute the same way. Point `controller.agentImage` at it on v0.9.12 (python agents), or both image values on the v0.10 release candidates.

```text
quickstart — one pod, nothing else to deploy

helm controller.agentImage = appa-kagent-quickstart
     (v0.10: also controller.goAgentImage)
        ▼
┌─ agent pod ───────────────────────────────────────────┐
│  entrypoint: APPA_RUNTIME_URL set → exec the gated    │
│    runtime against that shared appa-runtime; else     │
│    start appa-runtime on 127.0.0.1:8787, then exec    │
│    the runtime the rendered config matches            │
│                                                       │
│  kagent runtime (python or go) + AppaPluginKagent     │
│    │  POST http://127.0.0.1:8787/hook                 │
│    ▼                                                  │
│  appa-runtime · policy · Engine · appa.db — pod-local │
└───────────────────────────────────────────────────────┘
```

Quickstart limits:

- Trajectory state and `appa.db` live in the pod and die with it.
- A parent and a called agent run as two pods with two bundled runtimes, so their hooks land in two trajectories. Cross-workload correlation needs one `appa-runtime` that both reach: set `APPA_RUNTIME_URL` on both agents, and each pod's bundled runtime stays off. The demo chart does this for `cluster-ops` and `log-analyst`.
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
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: return a dict — ADK skips execution, and the dict becomes the function response the model reads. The feedback rides under `result`, the one key every kagent model converter serializes — any other shape reaches the model as an empty tool message. `Refuse` raises. | `functions.py` 509-534, 588-592 |
| `before_tool_callback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return None — the call passes through to `/mcp` on the runtime, which spends the vouch. A reviewed offer first raises ADK's tool confirmation and answers the model; the resumed call carries the person's `ruling` ([Human review](#human-review)) | `functions.py` 509-534 |
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
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: the returned dict skips execution and becomes the function response the model reads, its feedback under `result` — the key the kagent model converters serialize. `Refuse` raises. | `functions.py` 611-622 |
| `before_tool_callback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return None — the call passes through to `/mcp` on the runtime, which spends the vouch. A reviewed offer first raises ADK's tool confirmation and answers the model; the resumed call carries the person's `ruling` ([Human review](#human-review)) | `functions.py` 611-622 |
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

1. Read the mounted or env-delivered config.
2. Fill the OpenAI model's `reasoning_effort` from `APPA_KAGENT_OPENAI_REASONING_EFFORT` when the rendered config leaves it unset. The v1alpha2 `ModelConfig` enum admits `minimal`, `low`, `medium` and `high`, and no `none`; the gpt-5.6 models this integration runs against (luna, sol, terra) refuse function tools on chat completions unless the request carries `reasoning_effort: "none"`; the demo chart sets the env to `none` for its `gpt-5.6-luna` agents. A value the CRD set wins, and a model of another type is untouched. The go main applies the same env to `Model.ReasoningEffort`.
3. Refuse unknown fields, and the named out-of-band fields the runtime cannot gate, with an unready exit (see [Out-of-band flows](#out-of-band-flows)). Then validate with `AgentConfig.model_validate` and build the factory over `to_agent`.
4. Bring each out-of-band ADK feature under a mapped hook: wrap `code_executor` and the memory auto-save callback so both cross the tool gate, and constrain the compaction summarizer (see [Out-of-band flows](#out-of-band-flows)).
5. Rebuild the stock plugin list with the stock conditions (STS token propagation, LLM passthrough), then append `AppaPluginKagent(APPA_RUNTIME_URL)`.
6. Append the reserved-tool toolset: a `McpToolset` over streamable HTTP at `$APPA_RUNTIME_URL/mcp` (see [Remedy-plan execution](#remedy-plan-execution)).
7. Construct `KAgentApp(...)`, call `.build()`, and serve on the controller-given host and port — the same calls as [cli.py#L88-L101](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L88-L101).

Plugin order is load-bearing. ADK runs plugin callbacks in registration order, and it stops at the first non-`None` return. Appending `AppaPluginKagent` last is therefore safe only while no stock plugin answers a gated callback. The stock set is `ADKTokenPropagationPlugin` and `LLMPassthroughPlugin`, and neither answers one. An equivalence test asserts this per version, so a new stock plugin that gates cannot silently precede `AppaPluginKagent`.

### Out-of-band flows

Three ADK features on the python runtime move a value without a `FunctionTool` call, so `before_tool_callback` never sees them. The model callbacks stay liveness gates and do not gate the content. The entrypoint brings each feature under a mapped hook, constrains it, or refuses it. The go runtime wires none of the three.

- **Code execution — gated on cells A-py and B1-py.** `executeCodeBlocks` sets `code_executor` to a `SandboxedLocalCodeExecutor` ([v0.9.12 types.py#L509](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L509)). ADK runs it through the code-execution processor, not the tool path. The entrypoint wraps `agent.code_executor`. The wrapper sends a `ToolCall` for the code, runs the inner executor only on `AllowCall`, and returns the output through a `ToolResult`. Code execution then crosses the tool gate, and its output crosses at `ToolResult`. Main drops the feature, so cell B2-py needs no wrapper.
- **Memory write-back — gated on the python cells.** A memory config adds an `after_agent_callback` that persists the session every few turns ([v0.9.12 types.py#L585](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L585)). The read tools (`load_memory`, `save_memory`, `prefetch_memory`) stay ordinary tools and already cross the gate. The entrypoint wraps the persist callback: it sends a `ToolCall` for the persist and calls the stock callback only on `AllowCall`. An equivalence test pins the wrapped callback per version.
- **Context compaction — constrained on the python cells.** A compaction `summarizer_model` sends turn history to a summarizer model and injects the summary into later attention ([v0.9.12 types.py#L345](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L345)). APPA holds model calls as liveness gates, so the current model cannot gate the summarizer as a flow. The entrypoint constrains the summarizer to the agent model, and refuses a summarizer that names a different model or egress. The summary re-injection stays ungated. The spec closes it when it defines a summarization sink.

The refusal set is explicit. The strict schema refuses unknown fields. The entrypoint also refuses each named field it can neither gate nor constrain. Those are a divergent compaction `summarizer_model`, and any field that wires a model-native tool or a code path outside the mapped hooks. It classifies `share_tools` at build time, and refuses it when it wires a model-native tool. The entrypoint gates, constrains, or refuses every ADK feature that opens an out-of-band flow, so none runs unseen.

### Go — `AppaPluginKagent` on the Go ADK (verified against the locked adk/v2)

`appa-kagent-adk-go` is a small runtime main. It imports the public kagent `go/adk` packages and constructs the same agent the stock Go runtime builds from the rendered config. Then it registers the Go `AppaPluginKagent` through the ADK v2 plugin API. That is the registration point kagent itself uses ([rc4 adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)). It emits the same adapter wire as the python plugin, and it appends the reserved-tool toolset at construction.

The build composes upstream. `appa-kagent-adk-go` is a separate Go module with one runtime main. Its `go.mod` requires the `github.com/kagent-dev/kagent/go` module, which exports the `go/adk` packages. It also requires `google.golang.org/adk/v2`. Both are pinned, fetched unmodified from the module proxy, and locked by `go.sum`. `go build` links `AppaPluginKagent` into one static binary, and the image ships that binary under the stock args, port, and readiness contract. Go compiles the plugin list in. So the Go image adds its plugin at build time, and the python image adds its plugin at container start. The mapping verification also confirms the main uses only exported construction calls — the module imports no kagent `internal/` package.

#### adk/v2 v2.1.0 and v2.2.0 — cells B1-go and B2-go

The plugin surface is the same at both tags, so one table serves both cells. A `plugin.Plugin` exposes 12 callbacks through its accessors ([plugin/plugin.go#L113-L158](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L113-L158)). No run-error and no agent-error callback exists, so the error-turn gap of google-adk 1.31.1 applies to the go cells too. Signature references are v2.1.0 — v2.2.0 shifts the `llmagent.go` lines by two and changes nothing in the set.

| Go callback | HookEvent | Behavior, verified in [appa-kagent-adk-go/VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md) | Signature |
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

Verification status: the python tables are proven against their pinned wheels, and the go table is proven against `adk/v2` v2.1.0 and kagent rc4 in [appa-kagent-adk-go/VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md), row by row, with the plugin's tests exercising each row against a scripted `/hook` server. Three go-only behaviors differ from python and the plugin handles each: a `BeforeToolCallback` error becomes the model-facing error rather than aborting the run (the tool still never runs); `OnToolErrorCallback` and `AfterToolCallback` both fire on error paths, so the plugin self-recognizes its own fail-closed marker to avoid double-reporting one dispatch; and a deferred long-running tool reaches after-tool with no result, reported as an `indeterminate` outcome. The Go runtime also serves Foundry-model agents, which the compiler ties to the Go runtime. Deliverables: both tags (`<tag>` and `<tag>-full`, the variant kagent resolves for agents with skills or `executeCodeBlocks`).

In-process sub-agents on the go cells delegate through `transfer_to_agent`, an ordinary tool. The model call to it crosses `BeforeToolCallback`, so APPA gates the delegation as a `ToolCall` with `spawn:false`. The target runs in the same session and replies in-session, so no `SpawnResult` crosses. This path differs from the cross-pod `AgentTool` of the mapping tables, which feeds `ChildStart` and `SpawnResult`. The go mapping verification covers it.

## Remedy-plan execution

A blocked call answers with feedback that quotes an offer id. The offered plan executes through `execute_remedy_plan` — the reserved MCP tool of the engine, runtime-supplied and identical for every harness, served at `$APPA_RUNTIME_URL/mcp` from process start ([appa-runtime/src/mcp.rs](../../appa-runtime/src/mcp.rs)). The runtime refuses a call no hook vouched for, and executing the act spends the vouch.

Delivery is one more construction delta in each entrypoint. The python entrypoint appends a `McpToolset` over streamable HTTP at `$APPA_RUNTIME_URL/mcp` with a request timeout of 300 s (`REMEDY_CALL_TIMEOUT_SECONDS`; ADK's default of 5 s fails a remedy execution at the client before a parked authority consult or a slow sanitizer returns, so the timeout must exceed the runtime's consult budget) — the same classes kagent-adk itself uses for CRD MCP tools ([v0.9.12 types.py#L223](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L223), [rc4 types.py#L224](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/python/packages/kagent-adk/src/kagent/adk/types.py#L224)). The go main appends an `HttpMcpServerConfig` for `$APPA_RUNTIME_URL/mcp` to the rendered config's `HttpTools`, with the same 300 s request timeout (`remedyCallTimeoutSeconds`) — the stock path kagent builds every CRD MCP toolset from ([`tool/mcptoolset`](https://github.com/google/adk-go/tree/v2.1.0/tool/mcptoolset)). `AppaPluginKagent` answers the `ToolCall` hook of the reserved tool with `PassControl` and lets the call pass — the dedicated row in each mapping table above.

Coverage per plan element ([appa-engine/src/plan.rs](../../appa-engine/src/plan.rs)):

| Plan element | On kagent |
|---|---|
| `Authorize(authority)` | Executes engine-side inside the `execute_remedy_plan` call. A URL authority is consulted then; people out of band answer on their own channel, and a no-answer grants nothing and leaves the offer standing. A `hitl` authority: below. |
| `Accept(narrowing)` | Executes engine-side. The narrowed call redispatches through the normal gate. |
| `Sanitize(sanitizer)` | Executes engine-side, and the sanitized result returns through the mapped after-tool path. |
| `Derive(sanitizer)` | Executes engine-side — the progress hop. |
| `Redispatch` | Needs no id and no reserved tool. The agent calls the named tool, and the normal `ToolCall` gate applies. |
| fork advice | Advice, never a remedy. The spawn gate is already mapped. |

### Human review

`appa-runtime` consults its `hitl` authority through MCP elicitation inside the still-open `execute_remedy_plan` call ([appa-runtime/src/elicit.rs](../../appa-runtime/src/elicit.rs)) — the Claude Code channel. A kagent pod's MCP client carries no elicitation and has no person on it, so on kagent the person is reached through kagent's own approval flow, and the ruling returns to the runtime on the wire instead of through the elicitation:

1. The blocking decision carries the review. `deny_call` lists, per offer whose plan consults a `hitl` authority, the review as the person reads it (`review: [{offer_id, text}]`) — the same rendering the elicitation shows: the authority and its hint, the exact tool, the canonical arguments, and what the ruling covers. The engine builds it at the block ([`pending_reviews` in appa-runtime/src/engine.rs](../../appa-runtime/src/engine.rs)); the session keeps only the `hitl`-backed authorities.
2. The plugin asks through the stock confirmation. `AppaPluginKagent` remembers the reviews. When the agent calls `execute_remedy_plan` with a reviewed offer and no confirmation is on the call yet, the plugin requests ADK's tool confirmation with that text as the hint and answers the model that the reviewer has been asked — the run suspends, and the A2A caller decides. Every other remedy — accept the narrowing, a sanitizer, a human-less authority, a redispatch — the agent executes itself, steered by its instruction and the chat; no confirmation gate sits on the reserved tool.
3. The ruling rides the resumed control call. ADK re-runs the call with `tool_confirmation` set, for an approval and for a rejection alike, so the plugin's `tool_call` crosses with `ruling: approve` or `ruling: deny` ([appa-adapter-kagent](../../appa-adapter-kagent/src/lib.rs) parses it into `HookEvent::ToolCall { ruling }`). The runtime records it with the vouch and spends it as the `hitl` authority's answer for that one execution (`Backend::Hitl` in [appa-runtime/src/external.rs](../../appa-runtime/src/external.rs)): Approve is an approval and the call runs; Reject is a denial and retires every offer naming this authority for this exact call. kagent offers no "cancel", so no-answer arises only when the task is abandoned, and then the offer stands.

The answer never passes through the model: it crosses as ADK's confirmation and the plugin's wire field. What the person sees depends on the caller. Over A2A the confirmation request carries the review verbatim in `toolConfirmation.hint`, so an A2A client (a chat bot, an upstream agent) shows the consult artifact and nothing the model said. The v0.9.12 kagent dashboard renders the pending tool call and its arguments — `execute_remedy_plan` with the offer id — and does not show the hint; presenting the artifact in the dashboard is an upstream UI change. Verified live on kind, in the dashboard and over A2A: an approval runs the restart, a rejection leaves it blocked, and no other remedy raises a card ([e2e/ui](e2e/ui/), [e2e/a2a](e2e/a2a/)).

```text
human review on kagent — the person rules before the act;
the runtime spends the ruling inside it

model ─▶ restart_deployment
  │ ToolCall ─────────▶ appa-runtime: the plan consults
  │ ◀ DenyCall{feedback,   oncall (hitl); text = the
  │   review:[{offer_id,   consult artifact
  │   text}]}
model ─▶ execute_remedy_plan(offer_id)
  │ plugin: a reviewed offer, no confirmation on the call
  │   ─▶ ADK tool confirmation · hint = the review text
  │      kagent: Approve/Reject card · A2A input-required
  ·      the run ends · the person rules · a new run
  │ ToolCall{ruling: approve | deny} ─▶ rides the vouch
  │ ◀ PassControl
  │ /mcp execute_remedy_plan ─▶ Authorize(oncall)
  │      ruling on the vouch: spend it
  │      none: elicitation (Claude Code)
  │      neither: no answer, the offer stands
  │ ◀ Authorized | Declined
model ─▶ restart_deployment again ─▶ runs · or stays blocked
```

The go plugin carries the channel the same way. adk-go hands its `BeforeToolCallback` the tool context, so a reviewed control call raises the confirmation (`RequestConfirmation`, the review text as the hint) and the resumed call reads `ToolConfirmation()` into `ruling`; the plugin's `BeforeModelCallback` strips the `adk_request_confirmation` parts from the model's view, as kagent's own approval gate does ([plugin.go](appa-kagent-adk-go/plugin.go), tests in `plugin_test.go`). One more step is the go runtime main's: adk-go yields the reviewed call's pending response before the confirmation event, where python yields it after and its executor never converts it, so the main's `reviewShapedExecutor` drops that one response part from the A2A task. The dashboard renders the approval card only for a call without a response ([VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md)).

## Annotators

A `[[tool]]` entry either declares the complete contract or names a registered annotator, which answers it per proposed call. The consult runs engine-side inside the `ToolCall` round-trip, and the envelope carries only the annotator declaration and the artifact ([appa-runtime/src/consult.rs](../../appa-runtime/src/consult.rs)). The kagent wire already supplies the artifact ingredients: the tool name and the raw argument bytes. So annotators need no kagent surface, no plugin change, and no wire change. A no-answer renders as `Refuse` on the `ToolCall` hook, never as model-facing feedback ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs)) — the mapped `Refuse` leg in each table above.

Three kagent-specific notes:

- **Timeout budget.** The plugin `/hook` client timeout must exceed the runtime consult budget, or a slow annotator — endpoint, command, or model builtin — becomes a spurious fail-closed block. ADK has no callback timeout, so kagent carries no fail-open hazard: a slow consult costs latency, bounded upstream by A2A client patience.
- **Tool naming and coverage.** An annotation pins to the canonical digest under the tool name as ADK dispatches it. `[[tool]]` entries and mandates must match that spelling for every toolset — an equivalence-test item below. Two spellings verified on a live cluster: an MCP tool crosses under its plain name, and an agent tool crosses as `<namespace>__NS__<agent name>` with underscores. The wildcard entry (`name = "*"`) is the recommended first posture for a fleet: CRD-declared toolsets produce a long tail the policy never names up front. Optional tooling: generate `[[tool]]` skeletons from the `Agent` and `RemoteMCPServer` resources in the cluster. The reserved `execute_remedy_plan` needs no entry — the runtime recognizes its own tool first.
- **Builtin provisioning.** Model-builtin annotators execute in the `appa-runtime` deployment ([appa-runtime/src/external.rs](../../appa-runtime/src/external.rs)). `builtin = "llm"` needs `[externals.llm]` and model egress from the runtime pod. `builtin = "claude-code"` needs the claude CLI where the runtime runs. The quickstart inherits the same needs, because it bundles the runtime.

## Wire and codec

Each plugin emits one JSON event per callback. The event carries the kind, trajectory ids, tool name, raw argument bytes, outcome, and value fields. That is the data the matching `HookEvent` variant needs. The `appa-adapter-kagent` Rust crate parses this wire into `HookEvent` and renders each `HookDecision` into the response the plugins enforce, through the `Codec` contract of `appa-runtime-api` (`parse`, `render`). The wire carries no policy meaning. Raw tool arguments cross as spelled, and the Engine canonicalizes them. The reserved tool crosses as spelled too — `execute_remedy_plan` — so the runtime recognizes its own tool and binds the vouch. Two fields serve the human-review channel: a `deny_call` carries `review`, the offers whose plans consult a `hitl` authority with the text the person reads, and a `tool_call` of the reserved tool may carry `ruling` (`approve` or `deny`; any other spelling is malformed and the event is refused), the person's answer the plugin obtained through kagent's confirmation ([Human review](#human-review)). The fixture rows `tool_call_reserved` and `tool_call_reserved_ruled` in [fixtures/wire-events.jsonl](fixtures/wire-events.jsonl) pin both shapes; the python plugin spells the reserved name once, `RESERVED_TOOL` in `wire.py`. One wire and one codec serve both runtime images.

### Wire obligations

The runtime enforces three orderings at event admission. A driver that speaks this wire — a plugin, a test harness, a replay tool — keeps them or is refused:

1. **Audience narrowing is deny-then-accept, never allow-then-narrow.** A proposal whose `delta.audience` would narrow the trajectory (an ops-only read into a public session) answers `deny_call` with the offer to accept the narrowing. The call proceeds only after `execute_remedy_plan` accepts it and the agent proposes the call again, through the normal gate. Scenario scripts expect that two-step, not a plain allow.
2. **The vouch precedes `/mcp`.** The runtime recognizes `execute_remedy_plan` before the session runs (`is_control_tool` in [appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs)): its `tool_call` answers `pass_control` and binds the vouch, and a `tools/call` on `/mcp` with no vouched `tool_call` before it is refused with `no live offer with this id exists` ([appa-runtime/src/mcp.rs](../../appa-runtime/src/mcp.rs)). The plugins send that `tool_call` from the before-tool point, so the order holds on the runtime images by construction; a driver that bypasses the plugin sends it itself. The `tool_result` the after-tool point reports for the control tool is absorbed with `ack` — no dispatch was opened.
3. **One call at a time, and every allowed call is closed.** An `allow_call` opens a dispatch, and until its `tool_result` (or `spawn_result`) arrives every later `tool_call` on that trajectory is refused with `a call is already outstanding; propose one call at a time` ([appa-runtime/src/api/mod.rs](../../appa-runtime/src/api/mod.rs), raised at admission in [appa-runtime/src/api/session.rs](../../appa-runtime/src/api/session.rs)). The plugins close each dispatch from the after-tool and tool-error points — success, failure, and `indeterminate` alike. Recovery is bounded to the turn: a `turn_end` closes a dispatch the harness never reported, and an outcome reported after that `turn_end` is refused. So a driver reports each outcome before it proposes the next call, and ends the turn it abandons.

### Labels and flow completeness

The contract triple — `delta`, `requires`, `emits` — is engine algebra, and no label crosses the wire in either direction. The engine narrows the trajectory label with `delta` when it admits a result. It checks `requires` — membership, `history`, and `attention` marks — against trajectory state at dispatch ([appa-engine/src/check.rs](../../appa-engine/src/check.rs)). It records `emits` into the effect ledger, and effects commit on `Success`, never on `Indeterminate`.

That algebra is sound only over the flows the runtime saw, so the runtime image keeps one invariant. Every value that enters model attention or leaves the agent crosses a mapped hook. If it cannot, an entrypoint wrapper brings it under one, or the entrypoint refuses the config. On kagent the list is closed:

- User input crosses at `Prompt`, before the session append.
- Tool and child returns cross at `ToolResult` and `SpawnResult`.
- Delegated entries cross at `ChildStart`.
- Memory read tools and artifact loaders are ordinary tools, so they cross the tool gate.
- Code execution and the memory write-back cross the tool gate through their entrypoint wrappers ([Out-of-band flows](#out-of-band-flows)).
- The entrypoint constrains the compaction summarizer to the agent model, and refuses any other out-of-band feature.
- The CRD-compiled instruction is static config, not a flow.

Two boundaries stay non-gated by design, and the invariant names them rather than hiding them. The agent reply leaves through the A2A event queue, which only `on_event_callback` sees, and that callback is a liveness gate — `TurnEnd` gates nothing, and the implemented model defines no emission event ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs)). The compaction summary re-enters attention without a hook. When the spec defines a response sink and a summarization sink, `on_event_callback` is the ready carrier on kagent, because it can replace events. That is a forward path, not current behavior. The liveness gates hold everything else when the `/hook` channel is down.

### Delegation and the fork

A child trajectory begins at its parent's label — trust and audience; attention is never trajectory state ([appa-engine/src/plan.rs](../../appa-engine/src/plan.rs): "a child begins at the same label, so a fork cures no requirement"). The fork advice a block carries says the rest. Delegate the call and the work that uses its result. Finish by returning nothing or a sanitized derivation: returning the raw value applies the same change to the parent. A raw return that would narrow the parent is not merged; the parent's gate withholds it with the parent's own offers, deny-then-accept ([Wire obligations](#wire-obligations)). The hooks map the fork's moments:

| Moment | Hook | Runtime |
|---|---|---|
| The parent calls the agent tool | `ToolCall{spawn: true}` | Prepares the fork; its seed is the parent's label at release. `AllowCall` carries a `spawn_binding` the kagent plugin does not need: the child binds to the one spawn in flight. |
| The child pod's first event | `ChildStart` (kagent's lineage headers in session state) | Opens the fork for the child and binds the pod to it; its later `ToolCall`/`ToolResult` land in the child trajectory. |
| The value returns | `SpawnResult` (`ChildEnd` stays unfed on kagent) | Rules at the parent's gate: `Ack` crosses it unchanged (a void return too), `ChildReturn` substitutes a bound return sanitizer's derivation, `Block` withholds it — a narrowing raw return stays withheld until the parent accepts the narrowing ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs), `return_decision`). An unforked or `indeterminate` spawn result answers as an ordinary tool result (`Ack`/`ReplaceOutput`). |
| The child's run ends | `TurnEnd` (child) | Closes a dispatch the child left open. |

```text
delegation — a child starts at its parent's label,
then diverges

parent trajectory · cluster-ops     label: trusted · public
  │  tool_call {spawn: true} ─▶ the runtime prepares a fork
  ▼
child trajectory · log-analyst      label: trusted · public
  │  child_start opens the fork     ◀ inherited
  │  get_pod_logs — suspicious ingress: its own gate,
  │  its own remedy, in its own trajectory
  ▼
child label narrows                 label: suspicious · public
  │  spawn_result ─▶ the value meets the PARENT's gate
  ▼
raw, narrows nothing ─▶ crosses · parent unchanged
raw, would narrow    ─▶ withheld with the parent's own offers
   accept it         ─▶ crosses · parent narrows
   take a sanitizer  ─▶ the derivation crosses · parent as was
   no remedy         ─▶ parent keeps its label
no value, or withheld ─▶ parent keeps its label
```

On the go cells the stock executor does not land the lineage headers in session state; the runtime main's session-service decorator lands them from the A2A call context on every `Get` and `Create`, so a delegated child classifies as a child there too, python-identical ([VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md)). The demo delegates `cluster-ops` → `log-analyst` with `confined_child_return = true`; the delegation case in both matrices asserts the injected instruction never reaches the operator through the child.

## Trajectory identity

- Root `TrajectoryId`: the ADK session id with a harness prefix, per the `appa-runtime-api` convention.
- Child classification: the plugin in the child pod reads the inbound kagent call metadata to recognize a delegated entry (on the v1alpha3 lane the executor lands it in session state — [_agent_executor.py#L212-L214](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L212-L214)). A delegated entry feeds `ChildStart`. A plain external entry feeds `SessionStart` and `Prompt`.
- A successful child reply arrives as `{"result": ...}` for Task replies and as a bare string for direct Message replies — the plugins handle both shapes.
- Parent and child run as separate workloads (Deployments on lane A/B1, Substrate Actors on B2), so the hooks of one trajectory come from two plugin instances. Both must reach the same `appa-runtime` — a per-pod runtime would split one trajectory into two logs.

Delegation needs a name. The runtime serves kagent under `SpawnCoverage::Declared` (`appa-runtime/src/api/mod.rs`; the binary picks it per adapter): a `ToolCall{spawn: true}` releases only under a contract written for the agent's wire name, and the wildcard, which covers every ordinary call the policy does not write, covers no spawn. An unnamed agent is denied before the engine sees the call, with `EventError::UndeclaredSpawn` as the model's feedback, and no child ever opens. An ordinary call to a tool nothing covers keeps its operational refusal; only the spawn gets a policy denial the model reads, because the model can act on it by not delegating. Claude Code keeps `SpawnCoverage::Wildcard`. The quickstart's packaged policy names no agent, so delegation is off there until a policy names one; the demo policy names the log analysts and deliberately not the release managers, and the blocked case in both matrices proves the denial on both cells (`appa-runtime/tests/kagent_spawns.rs` proves the rule against a wildcard policy).

## Known gaps and handling

| Gap | Lane / runtime | Handling |
|---|---|---|
| No callback at ADK session creation | all | `SessionStart` synthesizes at first invocation, sent before `Prompt`. A never-invoked session emits nothing and flows nothing. |
| No error-turn callback in google-adk 1.31.1 | A-py | Earlier error callbacks plus `Indeterminate` classification at recovery. Closed on the lane B python cells by the google-adk 2.8.0 error callbacks. |
| No error-turn callback in adk/v2 | B1-go, B2-go | v2.1.0 and v2.2.0 define no run-error and no agent-error callback. Recovery classifies the open dispatch `Indeterminate`, as on cell A-py. |
| Human review on remedy plans | none | Both plugins carry the person's ruling through kagent's stock confirmation ([Human review](#human-review)). |
| Go image name on stable | A-go | v0.9.12 has no Go-image knob; the image must be served under the name the controller derives from `controller.agentImage` (`…/golang-adk:<tag>`). A registry alias, or the chart's kind override, does it. |
| CRD in-process `sub_agents` on the python runtime | B2 / python | The entrypoint refuses the config instead of dropping children. `appa-kagent-adk-go` consumes them natively once verified. |
| Out-of-band ADK features (code exec, memory write-back, compaction) | python cells | The entrypoint gates code execution and the memory persist as `ToolCall`s, constrains the compaction summarizer to the agent model, and refuses what it can neither gate nor constrain. See [Out-of-band flows](#out-of-band-flows). |
| Non-gated emission: the agent reply and the compaction summary | python cells | Named by design. The reply leaves through `on_event` (liveness only). The summary re-enters attention ungated. A spec response sink and summarization sink close them, with `on_event_callback` as the carrier. |
| Pre-gate session-name metadata | python cells | Stock kagent creates the session with about the first 20 characters of the prompt as its name, before the `Prompt` gate runs. A blocked `Prompt` still leaves that name in the session store. The events history barrier holds, because the blocked bytes never enter the events list. The write is stock kagent code, so closing the metadata leak needs an upstream change. |
| BYO agents | all | Per-agent images outside any shared runtime image. Their authors add the one plugin line, in either language. |
| Sandbox kinds (`AgentHarness`, `SandboxAgent`) | all | Different subsystem, out of scope. |
| Upstream has no plugin config knob | all | The entrypoints replay stock behavior through public calls. CI pins kagent and ADK versions per lane and re-runs the equivalence checks on each bump. Propose an upstream plugin-loading knob to delete the duplication. |

## Demo chart

The demo is the Helm chart `appa-kagent-demo` ([demo/chart](demo/chart)) on cells A-py and A-go. Prerequisite: kagent 0.9.12 with `controller.agentImage` set to `appa-kagent-quickstart`, so every declarative agent in the cluster is gated. The chart installs:

- One `appa-runtime` pod with three containers. `appa-runtime` listens on `127.0.0.1:18787`, loopback only, by design. An nginx relay listens on `:18789` behind the Service `appa-runtime` and rewrites `Host` to `127.0.0.1:18787`, because the runtime's `/mcp` (rmcp) validates `Host`. The mock externals listen on `127.0.0.1:8081`, because a `url` binding takes cleartext http to loopback only; the Service `appa-demo-mocks` exposes the change board's side channel (`GET /pending`, `POST /decide`).
- The policy [demo/chart/files/demo.appa.toml](demo/chart/files/demo.appa.toml): sanitizers over `[externals.llm]`, the `runbook-readers` annotator, the human-less `release-window` URL authority, the `change-board` URL authority, and the `oncall` `hitl` authority. The change board is people out of band: the mock's `POST /approve` parks the consult until a ruling arrives on the side channel or the approval window closes (25 s, inside the runtime's 30 s consult timeout), then answers 504 — a no-answer, so the offer stands. `rollback_deployment` requires the change board's attention.
- The `demo-tools` Deployment, Service and `RemoteMCPServer` ([demo/Dockerfile](demo/Dockerfile)).
- Agents `cluster-ops`, `log-analyst` and `release-manager` (listed by `cluster-ops` and named by no contract, so every delegation to it is denied), and their twins `cluster-ops-go`, `log-analyst-go` and `release-manager-go` on `runtime: go` (`agents.go.enabled`), with `APPA_RUNTIME_URL` (the relay Service) and `APPA_KAGENT_OPENAI_REASONING_EFFORT` in `spec.declarative.deployment.env`. The `cluster-ops` instruction states the autonomy rule: choose the remedy yourself, prefer the sanitized result, else accept the change, follow the chat when it steers, and name the remedy taken.
- ModelConfig `appa-demo-model` over the Secret of the same name, so the dashboard's Models → Edit flow supplies the key after install (`openai.apiKey` or `openai.existingSecret`). The model is `gpt-5.6-luna`.
- A post-install and post-upgrade seed Job that replays sixteen captured transcripts into kagent's store through the controller API (`POST /api/sessions`, `POST /api/tasks`) under `uuid5` ids, so the dashboard opens with every case as a chat and a re-run changes nothing.

The default image references (`ghcr.io/archestra-ai/*` at the chart's `appVersion`) name the released images; a cluster without them builds and loads them from source (the chart README).

```text
demo chart — cells A-py and A-go · kagent v0.9.12
controller.agentImage = appa-kagent-quickstart
(runtime: go derives …/golang-adk = appa-kagent-adk-go)

Agents cluster-ops, log-analyst (+ the -go twins)
  │  APPA_RUNTIME_URL
  ▼
Service appa-runtime:18789
┌─ pod appa-runtime ────────────────────────────────────┐
│  relay    nginx :18789 ─▶ 127.0.0.1:18787,            │
│           Host rewritten to the loopback value        │
│  runtime  appa-runtime on 127.0.0.1:18787             │
│           policy from a ConfigMap · appa.db           │
│  mocks    127.0.0.1:8081 · annotator · release window │
│           · change board (side channel: Service       │
│           appa-demo-mocks:8081)                       │
└───────────────────────────────────────────────────────┘
demo-tools  Deployment + Service + RemoteMCPServer
seed Job    post-install, post-upgrade ─▶ kagent-controller
            /api/sessions, /api/tasks
```

## PR sequence

| PR | Change |
|---|---|
| 1 | `appa-adapter-kagent` Rust codec crate: wire parse to `HookEvent`, decision render, `Adapter` enum variant, unit tests against recorded wire fixtures |
| 2 | `appa-kagent-adk` Python package: `AppaPluginKagent` with the callback table, per-ADK deltas, fail-closed transport, liveness gates, deny-dict self-recognition, `PassControl` pass-through, and the human-review channel — `review` remembered from `deny_call`, the confirmation request on a reviewed reserved call, `ruling` on the resumed call |
| 3 | Python entrypoint: strict config schema and refusal rules, stock plugin parity, both config deliveries, the controller args contract, the `reasoning_effort` fill from `APPA_KAGENT_OPENAI_REASONING_EFFORT`, the reserved-tool toolset with its 300 s request timeout |
| 4 | Python OCI image with pinned base digest, SBOM, and provenance |
| 5 | Lane A end-to-end: kind cluster with the stable chart, `controller.agentImage` swap, parent-and-child scenario against one shared runtime |
| 5b | The demo as a Helm chart ([Demo chart](#demo-chart)): the shared runtime pod with its relay and mock externals, the demo tools, both agents, the ModelConfig over one Secret the dashboard can also fill, and the seed Job that replays every showcase case as a chat from captured transcripts. Installs into any kagent 0.9.12 cluster whose `controller.agentImage` is `appa-kagent-quickstart`; the go twins need the go image under the derived name. Verified on kind: install, seed, both matrices on both cells. |
| 6 | `appa-kagent-adk-go`: adk/v2 mapping verification, the Go plugin and runtime main, both image tags, the reserved-tool toolset |
| 7 | Lane B end-to-end: the B1 dual-knob swap on the release-candidate chart, and B2 Harness × AgentTemplate on the Substrate path |
| 8 | Optional: `appa-kagent-quickstart` bundled image — both runtime layers, packaged `appa-runtime`, example policy, the quickstart entrypoint |

Every PR lands in this repository. The optional upstream contribution (a plugin config knob) is independent and non-blocking.

## Verification matrix

Adapter tests (per runtime):

- Callback-to-event mapping for every table row, including spawn classification by tool type and both child-return shapes.
- Deny path: a `DenyCall` skips execution, reaches the model as the function response, and is not double-reported.
- Replace path: `ReplaceOutput` and `ChildReturn` substitution at the after-tool point.
- Pre-append barrier: a blocked `Prompt` leaves no trace in session history.
- Fail closed: runtime down at each callback blocks the action, and liveness gates hold model and emission callbacks.
- Pass through: the reserved `execute_remedy_plan` call proceeds untouched on `PassControl`, and the runtime refuses an unvouched `/mcp` call.
- Out-of-band gate: a `DenyCall` on the code-execution `ToolCall` skips the subprocess, and the code output crosses at `ToolResult`. A `DenyCall` on the memory-persist `ToolCall` skips `add_session_to_memory`.
- Compaction constraint: the entrypoint accepts a `summarizer_model` equal to the agent model, and refuses a divergent one at startup.
- Startup refusal: missing `APPA_RUNTIME_URL`, unknown config fields, `sub_agents` on the python runtime, and the out-of-band refusal set — a divergent compaction `summarizer_model`, and a `share_tools` value that wires a model-native tool.
- Args contract: the entrypoints accept the controller args and answer readiness at the stock endpoint.
- No link from the codec crate to `appa-runtime` or `appa-engine`, and no policy state in either plugin.

Equivalence tests:

- Each entrypoint output matches the stock counterpart for the same rendered config, minus the added plugin.
- Record the tool names each toolset dispatches, per ADK version. `[[tool]]` entries and mandates match that spelling.
- Re-run on every kagent and ADK version bump, per lane. The callback tables re-verify against the newly locked ADK.
- Plugin order: no stock plugin implements a callback `AppaPluginKagent` gates on, so appending it last never lets a stock plugin short-circuit a gated callback.

The live matrices span three dimensions — kagent version, runtime plugin (python or go, each against the ADK that kagent version locks), and driver (the dashboard in headless Chromium, or A2A `message/send` alone) — and every combination is a row. Each row runs the same seventeen conversations from [e2e/ui](e2e/ui/) and [e2e/a2a](e2e/a2a/) ([e2e/README.md](e2e/README.md) is the index and `e2e/run-matrix.sh` the runner). Only the kagent v0.9.12 rows run today; the v0.10 rows wait for that lane's stack.

| kagent | Cell | Runtime plugin | Driver | Status |
|---|---|---|---|---|
| v0.9.12 | A-py | python · google-adk 1.31.1 | dashboard | 17/17 |
| v0.9.12 | A-py | python · google-adk 1.31.1 | A2A | 17/17 |
| v0.9.12 | A-go | go · adk/v2 v2.1.0 | dashboard | 17/17 |
| v0.9.12 | A-go | go · adk/v2 v2.1.0 | A2A | 17/17 |
| v0.10.0-rc4 | B1-py | python · google-adk 2.8.0 | dashboard, A2A | not run yet |
| v0.10.0-rc4 | B1-go | go · adk/v2 v2.1.0 | dashboard, A2A | not run yet |
| main | B2-py, B2-go | python · google-adk 2.8.0, go · adk/v2 v2.2.0 | dashboard, A2A | not run yet |

End-to-end tests:

- Lane A: declarative python agent on a kind cluster with the stable chart and the `controller.agentImage` swap — gated tool calls, replaced results, blocked prompts.
- Lane B1: both image knobs swapped on the release-candidate chart — a python agent and a go agent gated side by side.
- Lane B2: `AgentTemplate` × `Harness` on the Substrate path — admission by selector, `KAGENT_CONFIG_JSON` delivery, the env-var cap respected.
- Cross-workload trajectory: parent and delegated child against one shared runtime — one trajectory, `ChildStart` and `SpawnResult` correlated.
- Remedy execution per plan element: accept-narrowing, authorize with a stock authority, sanitize, derive hop, and redispatch — each on a gated agent, with the vouch spent once per act. URL authorities both ways: human-less (release-window) and people out of band (the change board, a parked consult ruled through its own channel — approve, deny, unanswered).
- Human review: the plugin raises kagent's confirmation on the reviewed `execute_remedy_plan` call and no other remedy raises one; an approval from the caller re-runs the call with `ruling: approve` and the act executes; a rejection re-runs it with `ruling: deny`, the authority denies, and the offer is retired.
- Annotated tool: the consult happens once, the annotation pins to the canonical digest, and replay re-reaches the decision without a second consult.
- Annotator down: the gated call refuses at the `ToolCall` hook, and nothing model-facing crosses.
- Wildcard: a tool the policy never names routes through the wildcard annotator and runs annotated.
- Crash window: kill the agent workload between `ToolCall` and `ToolResult`, then make sure the runtime reports the dispatch `Indeterminate`.
- Error-turn window per cell: on cell A-py and both go cells, force an unhandled model failure and make sure recovery closes the turn at the next admitted event. On the lane B python cells, make sure the error callbacks feed the failure `TurnEnd`s.
- Out-of-band flows on cell A-py: a code-execution agent whose policy denies the code sees the subprocess skipped, and a memory agent whose policy denies the persist writes nothing to the memory backend.
- Scenario harness ([e2e/test_scenarios.py](e2e/test_scenarios.py), the cases in [demo/SCENARIOS.md](demo/SCENARIOS.md)): a real ADK agent loop with a scripted model, the real plugin, a real `appa-runtime` on the example policy, and the demo MCP tools — the openappa.com/playground cases in cluster-ops terms. The plugin package declares uv `cache-keys` over `src/**/*.py`, so `uv run --with <path>` rebuilds the wheel after a source edit.
- Live matrices on the Helm-installed stack ([e2e/ui](e2e/ui/), [e2e/a2a](e2e/a2a/)): seventeen cases each, the same conversations with a real model — through the dashboard in headless Chromium, and over A2A `message/send` alone. Both answer the `oncall` review both ways, play the change-board member on the mock's side channel (approve, deny, unanswered), and assert that no other remedy raises a confirmation. The A2A driver waits `APPA_A2A_DECISION_SETTLE` (2 s) before it answers a confirmation, because kagent persists the confirmation-request event concurrently with answering the request. `APPA_AGENT` (UI) and `APPA_A2A_URL` (A2A) point either matrix at the go twin. Verified 17/17 each on kind, on cell A-py and on cell A-go. The three steer-dependent cases (the configured default, accept, decline) carry one rerun, because the model sometimes picks another remedy; the gate's substance is asserted off the tool results either way.
- Quickstart pod: the `appa-kagent-quickstart` image starts `appa-runtime` on loopback with the packaged policy, waits for health, and execs the gated entrypoint, which serves its A2A card — one pod, nothing else to deploy.
