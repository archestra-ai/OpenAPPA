# kagent adapter implementation plan

`appa-adapter-kagent` is the Rust codec crate, a workspace crate compiled into `appa-runtime`. The runtime selects it through its closed `Adapter` enum ([appa-runtime/src/main.rs](../../appa-runtime/src/main.rs)) as `appa_adapter_kagent::codec()`.

The agent side wraps the kagent ADK runtimes and takes its names from them. The `appa-kagent-adk` python package (plugin + entrypoint) builds as the `ghcr.io/archestra-ai/appa-kagent-adk` image from its [Dockerfile](appa-kagent-adk/Dockerfile). The `appa-kagent-adk-go` Go module (plugin + runtime main) builds as the `ghcr.io/archestra-ai/appa-kagent-adk-go` image from its [Dockerfile](appa-kagent-adk-go/Dockerfile). The release workflow publishes both images, and three more, at the release version ([Delivery units](#delivery-units)). Operators can also build them from source. The crate name never names an image.

Both images read `APPA_RUNTIME_URL`, both emit the same adapter wire, and the one codec crate parses it.

## Source baselines

Stable release — the installed lane:

- kagent [`v0.9.12`](https://github.com/kagent-dev/kagent/releases/tag/v0.9.12) (2026-07-20), the `kagent.dev/v1alpha2` API the public docs describe.
- kagent-adk 0.3.0. google-adk 1.31.1 — the lock resolution. The constraint is `google-adk>=1.28.1,<2` ([pyproject.toml#L25](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/pyproject.toml#L25)). The google_adk-1.31.1 wheel verifies the python-side callback claims below (wheel citations give paths and lines inside it).
- Go ADK: `google.golang.org/adk v1.4.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/go.mod#L50)) — the v1 line, before the v2 plugin API.

Release-candidate line — mid-cutover, in two observed states:

- Tag [`v0.10.0-rc4`](https://github.com/kagent-dev/kagent/releases/tag/v0.10.0-rc4) (`af84a618`, 2026-08-26): still the v1alpha2 `Agent` → Deployment controller, plus the `controller.goAgentImage` value. Go ADK: `google.golang.org/adk/v2 v2.1.0` ([go.mod#L50](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/go.mod#L50)). The workspace lock resolves google-adk 2.8.0.
- Main at [`52cc4de2`](https://github.com/kagent-dev/kagent/commit/52cc4de2a044a5062d10c4f189d863937c1bb0f9) (2026-09-01): the tree removes the v1alpha2 Agent controller. Agents are `v1alpha3` `AgentTemplate` × `Harness` pairs compiled to Substrate Actors. Go ADK: `google.golang.org/adk/v2 v2.2.0`. Python lock: google-adk 2.8.0 ([uv.lock#L1118-L1119](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/uv.lock#L1118-L1119)).

The `adk/v2` plugin surface is the same at both tags. The file `plugin/plugin.go` matches byte for byte between [v2.1.0](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go) and [v2.2.0](https://github.com/google/adk-go/blob/v2.2.0/plugin/plugin.go). Every callback signature matches.

OpenAPPA: `appa-runtime-api` hook vocabulary (`appa-runtime-api/src/lib.rs`) and the `appa-runtime` `/hook` endpoint (`appa-runtime/src/main.rs`).

## Target matrix

Six cells. Every diagram, mapping table, and end-to-end test in this plan names its cell.

| Cell | kagent | Runtime | ADK lock | Delivery knob | Image |
|---|---|---|---|---|---|
| A-py | v0.9.12 | python (CRD default) | google-adk 1.31.1 | helm `controller.agentImage` | `appa-kagent-adk` |
| A-go | v0.9.12 | go (`spec.declarative.runtime: go`) | adk/v2 v2.1.0, inside the image | the name kagent derives from `controller.agentImage` | `appa-kagent-adk-go` |
| B1-py | v0.10.0-rc4 | python (opt-in — the default is go) | google-adk 2.8.0 | helm `controller.agentImage` | `appa-kagent-adk` |
| B1-go | v0.10.0-rc4 | go (default) | adk/v2 v2.1.0 | helm `controller.goAgentImage` | `appa-kagent-adk-go` (one tag, no `-full`) |
| B2-py | main `52cc4de2` | python | google-adk 2.8.0 | `Harness.spec.workload.image` | `appa-kagent-adk` |
| B2-go | main `52cc4de2` | go | adk/v2 v2.2.0 | `Harness.spec.workload.image` | `appa-kagent-adk-go` |

v0.9.12 has no Go-image knob, so for an agent with `runtime: go` the controller derives the image name from `controller.agentImage`. It replaces the last repository path segment with `golang-adk` and keeps the registry and tag.

Cell A-go serves `appa-kagent-adk-go` under that derived name. The image carries its own adk/v2 and the kagent `go/adk` packages. So the v1 Go ADK in that tree plays no part. The image talks to the session and task API of the v0.9.12 controller. The demo chart runs the cell against it.

## Architecture decision

Each runtime takes the plugin through a public surface it already ships:

- **Python runtime**: `KAgentApp(plugins=[...])` is a public constructor parameter of the published `kagent-adk` package ([v0.9.12 _a2a.py#L63](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L63)). ADK forwards it into its plugin manager. The stock kagent entrypoint registers its own plugins through it ([cli.py#L69-L79](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L69-L79)). The stock entrypoint keeps a closed plugin list, and no config adds to it. So the `appa-kagent-adk` image carries its own entrypoint. That entrypoint makes the same public calls and appends one plugin.
- **Go runtime**: The kagent Go runtime is the official Google Go ADK. On the release-candidate line it registers plugins through the ADK v2 plugin API. The kagent runner adapter itself passes `runner.PluginConfig{Plugins: ...}` ([v0.10.0-rc4 go/adk/pkg/runner/adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)). But the build compiles the list in, with no config knob. So `appa-kagent-adk-go` is a replacement runtime main, built on the public kagent `go/adk` packages, that registers `AppaPluginKagent` (Go) in that list.
- Delivery is always an image reference the operator already controls. The lanes below name the knob per tree.

Both plugins hold no policy state. They serialize callback moments into wire events, send them to `APPA_RUNTIME_URL`, and enforce the answered `HookDecision`. Their identity bookkeeping is the per-session id pin, and on Go the per-invocation scope and id maps. Beyond their transport and that bookkeeping, either plugin keeps one thing between callbacks: the `review` text a `deny_call` handed it. The plugin drops it when the ruling rides the resumed call ([Human review](#human-review)). Policy, the Engine, consults, remedy plans, trajectory state, recovery semantics, and `appa.db` live in `appa-runtime`.

## Delivery lanes

### Lane A — stable release (v1alpha2 `Agent` → Deployment)

The shipped controller reconciles every `Agent` into a plain Deployment + Service. The declarative runtime image is install configuration:

- **Python (the stable default runtime)**: `spec.declarative.runtime` defaults to `python` in the shipped CRD ([agent_types.go#L175](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L175)). Helm `controller.agentImage.{registry,repository,tag}` flows into the controller ConfigMap as `IMAGE_*` ([controller-configmap.yaml#L12-L18](https://github.com/kagent-dev/kagent/blob/v0.9.12/helm/kagent/templates/controller-configmap.yaml#L12-L18)). That value becomes the agent Deployment image. Setting it to `appa-kagent-adk` gates every python-runtime agent with zero agent changes.
- **Go**: v0.9.12 has no Go-image value and no Go-image controller flag. For an Agent with `runtime: go` the controller derives the image name from `controller.agentImage`. The last repository path segment becomes `golang-adk`, and the registry and tag stay (`…/golang-adk:<tag>`). Cell A-go serves `appa-kagent-adk-go` under that derived name, through a registry alias or the kind load the chart README shows. The v1 Go ADK in the v0.9.12 tree plays no part, because the image carries its own adk/v2.

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
│    config_guard — refuse unknown fields, then         │
│    AgentConfig.model_validate                         │
│    KAgentApp(plugins=[ ..stock.., AppaPluginKagent ]) │
│  kagent-adk 0.3.0 · google-adk 1.31.1 · 12 callbacks  │
└──────────────────────────┬────────────────────────────┘
                           ▼  POST $APPA_RUNTIME_URL/hook
```

Rollout: one `helm upgrade` re-renders every declarative python agent onto the OpenAPPA image, cluster-wide, with no double-run hazard. Staged rollout: `spec.declarative.deployment.imageRegistry` overrides the registry component per agent ([agent_types.go#L392](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L392)). Set pilot agents to a registry that serves the OpenAPPA image under the stock repository path. Agents read `APPA_RUNTIME_URL` from `spec.declarative.deployment.env` ([agent_types.go#L443-L445](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L443-L445)) or from a baked image default.

### Lane B — release-candidate line

Two observed states, one adapter story:

**B1 — the current release candidates (v1alpha2 `Agent` → Deployment, both image knobs).** Same Deployment path as lane A, with three differences. The runtime default is `go` ([rc4 agent_types.go#L235-L241](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/api/v1alpha2/agent_types.go#L235-L241)). The value `controller.goAgentImage.{registry,repository,tag}` exists beside `controller.agentImage` ([rc4 controller-configmap.yaml#L28](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/helm/kagent/templates/controller-configmap.yaml#L28), [app.go#L226](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/pkg/app/app.go#L226)). And agents with skills or `executeCodeBlocks` resolve a `<tag>-full` variant of either runtime image ([rc4 deployments.go#L211](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/deployments.go#L211)). Neither OpenAPPA image carries a `-full` variant. Those agents resolve a name the images do not serve, so the gate does not cover them on cells B1-py and B1-go.

Set both values to the matching images to gate every other declarative agent. Then `appa-kagent-adk` gates the python agents, and `appa-kagent-adk-go` gates the go agents, except the agents that resolve the `-full` variant. Foundry-model agents require the Go runtime by compiler validation ([rc4 compiler.go#L224-L227](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/compiler.go#L224-L227)). The Go image `appa-kagent-adk-go` covers them, because it is a Go ADK runtime. Both runtimes receive the same args and keep the lane A readiness contract ([rc4 deployments.go#L176-L181](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/deployments.go#L176-L181), [manifest_builder.go#L569](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/core/internal/controller/translator/agent/manifest_builder.go#L569)).

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
  │  skills or executeCodeBlocks resolve <tag>-full,
  │  a variant this image does not carry (not covered).
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

- The CRD requires `workload.image` and pins it by digest ([harness_types.go#L34-L40](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/api/v1alpha3/harness_types.go#L34-L40)). The Harness `spec.env` carries `APPA_RUNTIME_URL`, with Secret refs available.
- Pairing: the controller matches each `AgentTemplate` against every same-namespace Harness selector ([collections.go#L85-L102](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/controller/collections.go#L85-L102)). It reconciles one Actor per pair. Rollout is "move the label match", never "add a second match" — a template matched by two Harnesses runs twice. Make old and new selectors disjoint.
- Config arrives as `KAGENT_CONFIG_JSON` / `KAGENT_AGENT_CARD_JSON` env ([actor_template.go#L43-L44](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L43-L44)). The python entrypoint calls the stock `kagent.adk.cli.materialize_from_env` when the installed kagent-adk defines it. That function handles both deliveries and is a no-op on the Deployment path. The Actor serves on port 8081 ([actor_template.go#L74](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L74)). The Substrate path caps an Actor at 32 env vars ([actor_template.go#L50-L52](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L50-L52)). The adapter needs one.
- Prerequisites: helm `controller.substrate.enabled=true`, the `ate-system` install, and a `WorkerPool` — the stock Substrate-path requirements.
- Templates whose compiled config carries in-process `sub_agents` ([kagent/compiler.go#L172](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/kagent/compiler.go#L172)) refuse on both images. Stock parsing drops them silently, and the adapter must not. The python runtime has no in-process sub-agent field. Python multi-agent uses `remote_agents`, and the runtime adds them as tools, so the tool gate already covers them. This refusal therefore guards a runtime mismatch, not a python feature. The refusal fires when a Go-compiled config with in-process children reaches the python image. Upstream rejects out-of-process (`Dedicated`) sub-agent tools ([compiler.go#L149-L151](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/translator/compiler.go#L149-L151)). The Go image `appa-kagent-adk-go` compiles against the rc4 schema, whose `AgentConfig` has no `sub_agents` field. Its main reads the raw `config.json` once ([configguard.go](appa-kagent-adk-go/cmd/appa-kagent-adk-go/configguard.go)). It exits 1 on `sub_agents`, on `agent_plugins`, and on any other top-level key outside that schema. It decodes the bytes it checked through the stock decoder and runs that decoded config. It exits 1 on a value the Go runtime would ignore: `execute_code` true, or a `context_config` that is not null. The stock loader alone drops such keys, ignores such values, and runs a narrower agent than declared.
- Cells B2-py and B2-go read against main `52cc4de2` only, and the [verification matrix](#verification-matrix) records them as not run.

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
│  go main materializes KAGENT_CONFIG_JSON, refuses     │
│    sub_agents, agent_plugins, any key outside the     │
│    rc4 schema and any value the go runtime would      │
│    ignore (exit 1), then registers                    │
│    runner.PluginConfig{Plugins:                       │
│      [ ..stock.., AppaPluginKagent ]}                 │
│  adk/v2 v2.1.0 inside the image · the tree locks      │
│  v2.2.0 — the same plugin surface                     │
└──────────────────────────┬────────────────────────────┘
                           ▼  POST $APPA_RUNTIME_URL/hook
```

## Runtime adapters

### Python — `AppaPluginKagent` on google-adk (verified)

The plugin implements google-adk `BasePlugin`. Each gated callback sends one wire event to `POST $APPA_RUNTIME_URL/hook` and enforces the decision the runtime answers. Five moments differ. The first user message of a run sends the session start, or the child start for a delegated entry. The prompt follows it. The before-tool callback on a reviewed `execute_remedy_plan` call raises the confirmation on its first pass and sends nothing. The after-tool callback sends nothing when its result is the deny or review dict of the before-tool callback. On the Go runtime the after-tool callback sends nothing on an error path, because the tool-error callback reported the failure. On the Go runtime a call gate that failed closed already reported. Then the tool-error and the after-tool callbacks both send nothing.

Every `turn_end` post is best-effort: a transport failure logs a warning and never raises. On every other post, a transport failure or a non-contract response raises. ADK then wraps the exception and aborts the invocation (`plugin_manager.py`: 288-305 in 1.31.1, 316-322 in 2.8.0).

One mapping table per locked ADK version follows. Wheel citations name paths and lines inside the wheel for that version.

#### google-adk 1.31.1 — cell A-py

The 1.31.1 wheel defines 12 lifecycle callbacks. Their `base_plugin.py` lines are 114, 136, 155, 174, 198, 217, 233, 253, 272, 297, 321, 348.

| ADK callback | HookEvent | Enforcement at the callback | 1.31.1 wheel evidence |
|---|---|---|---|
| `on_user_message_callback`, the first invocation in a run | `SessionStart` for a fresh root session, or `ChildStart` on every delegated entry | `Refuse` raises before the plugin sends `Prompt`. A `Context` answer carries the return contract. The plugin prepends that text to the first user message of the child. A repeated start of the same pair is a resume the runtime records | `runners.py` 1537-1541 |
| `on_user_message_callback` | `Prompt` | `Ack` only: the runtime notes the previous turn as ended and gates no prompt text. The callback fires before the session append | fires before the append: `runners.py` 1537 then 1550-1556 |
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: return a dict — ADK skips execution, and the dict becomes the function response the model reads. The feedback rides under `result`, a key every kagent model converter serializes (the converters also read a `content` text list). Any other dict reaches the OpenAI and Ollama converters as an empty tool message. A `Refuse` raises. | `functions.py` 509-534, 588-592 |
| `before_tool_callback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return None — the call reaches `/mcp` on the runtime, which spends the vouch. A reviewed offer first raises the ADK tool confirmation and answers the model. The resumed call carries the `ruling` of the person ([Human review](#human-review)) | `functions.py` 509-534 |
| `after_tool_callback` | `ToolResult` | `ReplaceOutput{output}`: return a dict — it replaces the result the model sees. `Block`: return a withheld dict — the model reads `[appa] the tool result was withheld: <reason>`. A call the plugin already answered — a deny, a review, or a failure — is closed at that moment and reported once. The plugin remembers the call by its `function_call_id`, so a tool result that spells the deny marker itself cannot skip this gate. | `functions.py` 547-576 |
| `on_tool_error_callback` | `ToolResult` with `Failure` outcome | `Ack`: return None, and the original error propagates. `ReplaceOutput` or `Block`: return a dict (the withheld text on `Block`). The dict becomes the function response the model reads. Does not fire for a `before_tool_callback` raise — a `Refuse` stays terminal. | `functions.py` 535-545 |
| `before_tool_callback` on an agent tool | `ToolCall{spawn:true}` | The runtime marks the call a spawn. It holds the call until this session declares what a return may carry. The plugin declares the bare floor itself, so the model never reads that block ([Delegation and the fork](#delegation-and-the-fork)). The released `AllowCall` carries a `spawn_binding` the plugin drops: the child binds to the one spawn in flight | `functions.py` 509-534 |
| `before_agent_callback` (local sub-agent) | `ChildStart` | any answer but `Ack` raises, and ADK aborts the invocation before the child body runs. A returned `Content` is the ADK exit the plugin does not use. A `Context` answer raises too, because a local scope has no first user message for the return contract. Both images refuse in-process sub-agents, so no gated config reaches this row. The agent that owns the invocation gets a liveness gate instead | `base_agent.py` 288-296, 447-452 |
| `after_agent_callback` (local sub-agent) | `TurnEnd` (child) | a best-effort child `turn_end`, never raises. The agent that owns the invocation gets a liveness gate instead | `base_plugin.py` 217 |
| `after_tool_callback` on the agent-tool return | `SpawnResult` | The replay point of the parent. `Ack`: return None, and the parent reads the reply of the child unchanged, because that value already crossed. `Block`: the withheld dict, and the model reads the reason. Nothing crosses here that did not cross at the child, so a replay answers `Ack` and any other value answers `Block`. The plugin enforces a `ChildReturn` here anyway | `functions.py` 547-576 |
| `before_model_callback` (child scope) | none | registers its return-gate tool on the request ADK rebuilds each step. The dispatch of the call below then resolves it | `llm_request.py` 245-275 |
| `after_model_callback` (child scope) | none | replaces the final text of the child with one call to that return-gate tool, carrying the text. The plugin callbacks run first, and the flow finalizes and dispatches the replaced response | `base_llm_flow.py` 284, 1210, 971 |
| the return-gate tool at `before_tool_callback` | none | APPA owns the tool. The plugin matches it by exact name and posts nothing, so the runtime opens no dispatch for it. The reserved `execute_remedy_plan` tool is the other case: the plugin posts that `ToolCall`, and the runtime absorbs the call by name | `functions.py` 509-534 |
| the return-gate tool body (child scope) | `ChildEnd` | posts the final message of the child. `Ack`: it crossed, and the child stops with it. `ChildReturn{value}`: the plugin posts a second `child_end` with those exact bytes, and the child stops with them. `Block{reason}`: the reason becomes the tool result, and the model writes another final message this gate holds the same way | `functions.py` 547-576 |
| the return-gate tool at `after_tool_callback` | none | the plugin recognizes the gate object it owns, not the name, and posts nothing. The `ChildEnd` above is the only event this moment produces. A `ToolResult` here would name no open dispatch, so the runtime would answer `Block` and the child could not stop | `functions.py` 547-576 |
| `after_run_callback` | `TurnEnd` (root, or the delegated child pod) | observe — a best-effort post. Also fires after a `before_run` halt | `runners.py` 843-861, 952 |
| `before_run_callback` | none | liveness gate (a `ping`): raises when the `/hook` channel is down — the same bytes crossed at `Prompt` | `runners.py` 843-861 |
| `before_model_callback`, `after_model_callback`, `on_model_error_callback`, `on_event_callback` | none | liveness gates: raise when the `/hook` channel is down, pass otherwise. In a child scope the two model points also carry the return gate above | `base_plugin.py` 233, 253, 272, 155 |

The value of a child crosses at `ChildEnd`, at the stop of that child. The plugin holds the stop of every child scope. The after-model point replaces the final text with the return-gate call, and the gate posts `child_end`. A return the parent did not declare comes back as the tool result of the gate. The model then writes another final message. That is the second attempt the blocking stop of Claude Code gives. The outgoing A2A reply of the child carries the value that crossed, byte for byte, so the `spawn_result` of the parent replays it.

1.31.1 notes: no error-turn callback exists — `on_run_error_callback` and `on_agent_error_callback` are absent from the wheel. ADK skips `after_run_callback` when a run dies on an unhandled error (`runners.py` 949-954, no `finally`). The model-error and tool-error callbacks catch the common failures earlier. The `appa-runtime` recovery classifies the rest `Indeterminate` at the next `turn_end`, or at the first `tool_call` after the next prompt. An `AgentTool` child runs under a fresh child Runner. That runner inherits the plugin list of the parent, so its calls still cross the gate. It carries a fresh session and no lineage headers, so the child opens its own root trajectory and binds no fork ([Known gaps](#known-gaps-and-handling)).

#### google-adk 2.8.0 — cells B1-py and B2-py

The 2.8.0 wheel defines 14 lifecycle callbacks: the twelve above at the same `base_plugin.py` lines, plus `on_agent_error_callback` (374) and `on_run_error_callback` (394). The shared rows keep the 1.31.1 semantics, re-verified at the 2.8.0 sites:

| ADK callback | HookEvent | Enforcement at the callback | 2.8.0 wheel evidence |
|---|---|---|---|
| `on_user_message_callback`, the first invocation in a run | `SessionStart` for a fresh root session, or `ChildStart` on every delegated entry | `Refuse` raises before the plugin sends `Prompt`. A `Context` answer carries the return contract. The plugin prepends that text to the first user message of the child. A repeated start of the same pair is a resume the runtime records | `runners.py` 677 |
| `on_user_message_callback` | `Prompt` | `Ack` only: the runtime notes the previous turn as ended and gates no prompt text. The callback runs at 677, before the session append at 687-689 | `runners.py` 677, 687-689 |
| `before_tool_callback` | `ToolCall{spawn}` | `DenyCall{feedback}`: the returned dict skips execution and becomes the function response the model reads. Its feedback rides under `result`, a key the kagent model converters serialize. A `Refuse` raises. | `functions.py` 611-622 |
| `before_tool_callback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return None — the call reaches `/mcp` on the runtime, which spends the vouch. A reviewed offer first raises the ADK tool confirmation and answers the model. The resumed call carries the `ruling` of the person ([Human review](#human-review)) | `functions.py` 611-622 |
| `after_tool_callback` | `ToolResult` | `ReplaceOutput{output}`: the returned dict replaces the result. `Block`: a withheld dict — the model reads `[appa] the tool result was withheld: <reason>`. A call the plugin already answered is closed by its `function_call_id` and reported once, so a tool result that spells the deny marker itself cannot skip this gate. | `functions.py` 652-656 |
| `on_tool_error_callback` | `ToolResult` with `Failure` outcome | `Ack`: return None, and the original error propagates. `ReplaceOutput` or `Block`: return a dict (the withheld text on `Block`). The dict becomes the function response the model reads | `functions.py` 544-563, 595, 641 |
| `before_tool_callback` on an agent tool | `ToolCall{spawn:true}` | as `ToolCall` on the A-py row: the runtime holds the marked spawn, and the plugin declares the bare floor before it proposes the call again | `functions.py` 611-622 |
| `before_agent_callback` (sub-agent) | `ChildStart` | any answer but `Ack` raises, and ADK aborts the invocation before the child body runs. A returned `Content` is the ADK exit the plugin does not use. Sub-agents re-enter `run_async`, so this fires per child. A `Context` answer raises, because a local scope has no first user message for the return contract. The agent that owns the invocation gets a liveness gate instead | `base_agent.py` 320, 382 |
| `after_agent_callback` (sub-agent) | `TurnEnd` (child) | a best-effort child `turn_end`, never raises. The agent that owns the invocation gets a liveness gate instead | `base_plugin.py` 217 |
| `after_tool_callback` on the agent-tool return | `SpawnResult` | the replay point of the parent, as on the A-py row | `functions.py` 652-656 |
| `before_model_callback` (child scope) | none | registers its return-gate tool on each rebuilt request | `llm_request.py` 287-325 |
| `after_model_callback` (child scope) | none | replaces the final text of the child with one call to that tool. The plugin callbacks run first, and the flow dispatches the replaced response | `base_llm_flow.py` 340-347, 1824-1831, 1492-1501 |
| the return-gate tool at `before_tool_callback` | none | APPA owns the tool. The plugin matches it by exact name and posts nothing, so the runtime opens no dispatch for it | `functions.py` 611-622 |
| the return-gate tool body (child scope) | `ChildEnd` | posts the final message of the child and enforces the answer, as the A-py rows state | `functions.py` 652-656 |
| the return-gate tool at `after_tool_callback` | none | the plugin recognizes the gate object it owns, not the name, and posts nothing. A `ToolResult` here would name no open dispatch, so the runtime would answer `Block` and the child could not stop | `functions.py` 652-656 |
| `on_agent_error_callback` | `TurnEnd` (the child scope, or the root for the owning agent) | observe — a best-effort post, and the error still propagates. The wire carries no outcome on a `TurnEnd` | `base_agent.py` 632 |
| `on_run_error_callback` | `TurnEnd` (root) | observe — a best-effort post. ADK treats the callback as notification and suppresses anything it raises, so this point cannot hold. A root error sends `turn_end` from both error callbacks, and the runtime answers each with `Ack` | `runners.py` 96-108, 786-790 |
| `after_run_callback` | `TurnEnd` (root, or the delegated child pod) | observe — a best-effort post. Also fires after a `before_run` halt | `runners.py` 791 |
| `before_run_callback` | none | liveness gate (a `ping`): raises when the `/hook` channel is down — the same bytes crossed at `Prompt` | `base_plugin.py` 136 |
| `before_model_callback`, `after_model_callback`, `on_model_error_callback`, `on_event_callback` | none | liveness gates: raise when the `/hook` channel is down, pass otherwise. In a child scope the two model points also carry the return gate above | `base_plugin.py` 233, 253, 272, 155 |

The two error rows close the error-turn gap on the lane B python cells. The gap stays open on the go cells, because adk/v2 has no error-turn callback (the go table below).

Entrypoint (python image), in order:

1. Read the mounted or env-delivered config.
2. Fill `reasoning_effort` on the OpenAI model from `APPA_KAGENT_OPENAI_REASONING_EFFORT` when the rendered config leaves it unset. The v1alpha2 `ModelConfig` enum admits `minimal`, `low`, `medium` and `high`, and no `none`. The gpt-5.6 models this integration runs against (luna, sol, terra) refuse function tools on chat completions without `reasoning_effort: "none"`. The demo chart sets the env to `none` for its `gpt-5.6-luna` agents. A value the CRD set wins, and a model of another type stays as it is. The go main applies the same env to `Model.ReasoningEffort`.
3. Refuse `sub_agents`, a tool named `appa_return`, unknown fields and a config that does not validate with exit 2 (see [Out-of-band flows](#out-of-band-flows)). The guard refuses `sub_agents` by name first. It refuses a config that declares `appa_return` too: that name belongs to the plugin's return gate, and a declared tool holding it would collide with the gate at dispatch. The refusal names the path that declares it. Then it validates the config with `AgentConfig.model_validate` and walks the raw config beside the validated instance. A config or agent card that does not validate exits 2 with one line. The line opens with `the config does not validate` for the card too. It carries the error count, then the location and the pydantic error type of the first error, and never a value. At each nested object it checks a raw key against the fields of the class pydantic built for that object. Every string alias of a field counts as a known key, and its bare name when pydantic reads it. A key pydantic reads through an `AliasPath` is unknown, because the walk cannot pair that nested dict with a model. kagent-adk v0.9.12 declares no `AliasPath`. The three TLS keys kagent lifts from the `params` of an MCP tool count too. For a discriminated union that class is the member pydantic chose. So a key of a sibling model variant is unknown, and the diagnostic names its path (`model.region`). Both TLS spellings on a model pass, because `tls_insecure_skip_verify` is a validation alias of `tls_disable_verify`. Then refuse a divergent compaction summarizer on the validated config with exit 2, and build the factory over `to_agent`.
4. Gate each out-of-band ADK feature through a mapped hook. Wrap `code_executor` and the memory auto-save callback so both cross the tool gate. Constrain the compaction summarizer (see [Out-of-band flows](#out-of-band-flows)).
5. Rebuild the stock plugin list with the stock conditions (STS token propagation, LLM passthrough), then append `AppaPluginKagent(APPA_RUNTIME_URL)`.
6. Append the reserved-tool toolset: a `McpToolset` over streamable HTTP at `$APPA_RUNTIME_URL/mcp` (see [Remedy-plan execution](#remedy-plan-execution)).
7. Construct `KAgentApp(...)`, call `.build()`, and serve on the controller-given host and port. Those are the same calls as [cli.py#L88-L101](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L88-L101).

Plugin order is load-bearing. ADK runs plugin callbacks in registration order, and it stops at the first non-`None` return. Appending `AppaPluginKagent` last is therefore safe only while no stock plugin answers a gated callback. The stock set is `ADKTokenPropagationPlugin` (`before_run`, `after_run`) and `LLMPassthroughPlugin` (`before_model`). Those three callbacks are liveness gates and the turn end in `AppaPluginKagent`, and each stock override returns `None`. In a child scope `before_model` also registers the return-gate tool, and the `LLMPassthroughPlugin` override in front of it returns `None`. No stock plugin overrides a gated callback: the user message, before and after tool, tool error, or before agent. On the v0.9.12 lane `test_equivalence.py` pins both halves. The test `test_no_stock_plugin_overrides_a_gated_callback` introspects the stock plugin classes. The test `test_the_gate_fires_behind_the_stock_plugins_in_a_real_runner` runs a real `InMemoryRunner` behind the stock plugins and sees the `tool_call` cross.

### Out-of-band flows

Three ADK features on the python runtime move a value without a `FunctionTool` call, so `before_tool_callback` never sees them. The model callbacks stay liveness gates and do not gate the content. The entrypoint gates each feature through a mapped hook, constrains it, or refuses it. The go runtime wires none of the three. The go guard refuses `execute_code` true and every `context_config` that is not null, the empty object included. The kagent controller warns on the same two features and renders them anyway, but its context warning covers the compaction case only. The go guard refuses a wider set than the controller reports.

- **Code execution** — gated on cells A-py and B1-py. The `executeCodeBlocks` field sets `code_executor` to a `SandboxedLocalCodeExecutor` ([v0.9.12 types.py#L509](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L509)). ADK runs it through the code-execution processor, not the tool path. The entrypoint wraps `agent.code_executor`. The wrapper sends a `ToolCall` for the code, runs the inner executor only on `AllowCall` or `PassControl`, and returns the output through a `ToolResult`. Code execution then crosses the tool gate, and its output crosses at `ToolResult`. Main drops the feature, so cell B2-py needs no wrapper.
- **Memory write-back** — gated on the python cells. A memory config adds an `after_agent_callback` ([v0.9.12 types.py#L585](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L585)). It persists the session every few turns. The tools `load_memory` and `save_memory` are ordinary tools and cross the gate. The prefetch is not: `prefetch_memory` ([v0.9.12 tools/prefetch_memory_tool.py#L117](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/tools/prefetch_memory_tool.py#L117)) searches the memory on the first turn and appends the hits to the model instructions, with no function call. The Go runtime installs the adk-go `preloadmemorytool` the same way. That ingress crosses no hook on either runtime ([Known gaps](#known-gaps-and-handling)). The entrypoint wraps the persist callback: it sends a `ToolCall` for the persist and calls the stock callback only on `AllowCall` or `PassControl`. A `DenyCall` skips the stock callback and sends no result. After the stock callback returns, the wrapper reports `{"persisted": true}` as the `ToolResult` of the persist. It enforces nothing on that answer, because the result of a persist never enters attention. The call gate is the enforcement point. A `Refuse` on the report still raises, because the runtime answers it with HTTP 409.
- **Context compaction** — constrained on the python cells. A compaction `summarizer_model` sends turn history to a summarizer model ([v0.9.12 types.py#L345](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L345)). It injects the summary into attention for the following turns. APPA holds model calls as liveness gates, so the implemented model cannot gate the summarizer as a flow. The entrypoint constrains the summarizer to the agent model, and refuses a summarizer that names a different model or egress. The summary re-injection stays ungated, because the implemented model defines no summarization sink.

The refusal set is explicit. The config guard refuses unknown fields, `sub_agents`, and a divergent compaction `summarizer_model`. On the v0.9.12 `kagent-adk` the field `share_tools` does not exist, so the guard refuses it as an unknown key. The controller renders the TLS settings of a `RemoteMCPServer` inside `http_tools[].params` and `sse_tools[].params`. The kagent `_McpTlsMixin` lifts the three keys to the tool config, so the guard accepts them there. On the v0.10 line `share_tools` adds three ordinary function tools — `create_share_link`, `list_share_links`, `delete_share_link` (rc4 `go/adk/pkg/runner/adapter.go`) — and ordinary function tools cross the tool gate. The entrypoint gates or refuses every other ADK feature that opens an out-of-band flow. The compaction summary re-injection stays ungated (above).

### Go — `AppaPluginKagent` on the Go ADK (verified against the locked adk/v2)

`appa-kagent-adk-go` is a small runtime main. It imports the public kagent `go/adk` packages and constructs the same agent the stock Go runtime builds from the rendered config. Then it registers the Go `AppaPluginKagent` through the ADK v2 plugin API. That is the registration point kagent itself uses ([rc4 adapter.go#L93-L111](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/runner/adapter.go#L93-L111)). It emits the same adapter wire as the python plugin, and it appends the reserved-tool toolset at construction.

The build composes upstream. `appa-kagent-adk-go` is a separate Go module with one runtime main. Its `go.mod` requires the `github.com/kagent-dev/kagent/go` module, which exports the `go/adk` packages. It also requires `google.golang.org/adk/v2`. `go.mod` pins both, the module proxy serves them unmodified, and `go.sum` locks them. Then `go build` links `AppaPluginKagent` into one static binary. The image ships that binary under the stock args, port, and readiness contract.

Go compiles the plugin list in. So the Go image adds its plugin at build time, and the python image adds its plugin at container start. The mapping verification also confirms the main uses only exported construction calls — the module imports no kagent `internal/` package.

#### adk/v2 v2.1.0 and v2.2.0 — cells A-go, B1-go and B2-go

The plugin surface is the same at both tags, so one table serves all three go cells. A `plugin.Plugin` exposes 12 callbacks through its accessors ([plugin/plugin.go#L113-L158](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L113-L158)). No run-error and no agent-error callback exists. So the error-turn gap of google-adk 1.31.1 applies to the go cells too. Signature references are v2.1.0 — v2.2.0 shifts the `llmagent.go` lines by two and changes nothing in the set.

| Go callback | HookEvent | Behavior, verified in [appa-kagent-adk-go/VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md) | Signature |
|---|---|---|---|
| `OnUserMessageCallback` | `SessionStart` (or `ChildStart` for a delegated entry), then `Prompt` | fires before the session append, and a returned error aborts the run. The runtime answers `Prompt` with `Ack` only. A delegated entry sends `ChildStart` on every entry. A `Context` answer carries the return contract, which the plugin prepends to the first user message of the child. A repeated start of the same pair is a resume | [plugin.go#L161](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L161) |
| `BeforeToolCallback` | `ToolCall{spawn}` | a non-nil map skips execution. It reaches the model as the function response | [llmagent.go#L390](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L390) |
| `BeforeToolCallback` on `execute_remedy_plan` | `ToolCall` | `PassControl`: return a nil map. The call reaches `/mcp` on the runtime | [llmagent.go#L390](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L390) |
| `BeforeToolCallback` on an agent tool | `ToolCall{spawn:true}` | the runtime holds the marked spawn, and the plugin declares the bare floor itself before it proposes the call again ([Delegation and the fork](#delegation-and-the-fork)) | [llmagent.go#L390](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L390) |
| `AfterToolCallback` | `ToolResult`, or `SpawnResult` on an agent tool | a non-nil map replaces the result. The model sees the map | [llmagent.go#L399](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L399) |
| `OnToolErrorCallback` | `ToolResult` with `Failure` outcome | a map converts the error. A returned error stays terminal | [llmagent.go#L405](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L405) |
| `BeforeAgentCallback` | `ChildStart` | a returned `Content` ends the child before its body runs. The scope that owns the invocation gets a liveness gate. A `Context` answer has no channel in a local scope, and the plugin fails closed on it. Another scope sends `child_start` without a binding. The runtime binds it only to a spawn in flight. Otherwise it refuses the scope | [agent.go#L129](https://github.com/google/adk-go/blob/v2.1.0/agent/agent.go#L129) |
| `AfterAgentCallback` | `TurnEnd` (in-process child) | fires once per sub-agent scope that completes its body: a best-effort child `turn_end`. When the invocation ends inside the body or at a before-agent halt, adk-go skips the after callbacks ([agent.go#L202](https://github.com/google/adk-go/blob/v2.1.0/agent/agent.go#L202)). Recovery then closes the open dispatch at the next turn end or first tool call. The scope that owns the invocation gets a liveness gate | [agent.go#L137](https://github.com/google/adk-go/blob/v2.1.0/agent/agent.go#L137) |
| `BeforeModelCallback` (child scope) | none | registers its return-gate tool in the request adk-go rebuilds each step. The function-call dispatch then resolves it | [base_flow.go#L552-L562](https://github.com/google/adk-go/blob/v2.1.0/internal/llminternal/base_flow.go#L552-L562), [#L755-L762](https://github.com/google/adk-go/blob/v2.1.0/internal/llminternal/base_flow.go#L755-L762) |
| `AfterModelCallback` (child scope) | none | replaces the final text of the child with one call to that tool. The plugin callback runs first, and the replaced response reaches the function-call dispatch | [base_flow.go#L804-L820](https://github.com/google/adk-go/blob/v2.1.0/internal/llminternal/base_flow.go#L804-L820), [#L890-L898](https://github.com/google/adk-go/blob/v2.1.0/internal/llminternal/base_flow.go#L890-L898) |
| the return-gate tool at `BeforeToolCallback` | none | APPA owns the tool. The plugin matches it by exact name and posts nothing, so the runtime opens no dispatch for it | [llmagent.go#L390](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L390) |
| the return-gate tool body (child scope) | `ChildEnd` | posts the final message of the child and enforces the answer, as the python rows state | [base_flow.go#L596-L623](https://github.com/google/adk-go/blob/v2.1.0/internal/llminternal/base_flow.go#L596-L623) |
| the return-gate tool at `AfterToolCallback` | none | the plugin recognizes the gate object it owns, not the name, and posts nothing. A `ToolResult` here would name no open dispatch, so the runtime would answer `Block` and the child could not stop | [llmagent.go#L399](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L399) |
| `AfterRunCallback` | `TurnEnd` (root, or the delegated child pod) | nothing — the signature returns no value, observation only. A best-effort post | [plugin.go#L165](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L165) |
| `BeforeRunCallback` | none | liveness gate — the same bytes crossed at `Prompt` | [plugin.go#L163](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L163) |
| `BeforeModelCallback`, `AfterModelCallback`, `OnModelErrorCallback`, `OnEventCallback` | none | liveness gates. In a child scope the two model points also carry the return gate above | [llmagent.go#L366-L378](https://github.com/google/adk-go/blob/v2.1.0/agent/llmagent/llmagent.go#L366-L378), [plugin.go#L167](https://github.com/google/adk-go/blob/v2.1.0/plugin/plugin.go#L167) |

Verification status: the pinned wheels verify the python tables. The file [appa-kagent-adk-go/VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md) verifies the go table against `adk/v2` v2.1.0 and kagent rc4, row by row. The plugin tests exercise each row against a scripted `/hook` server.

One Go-only behavior differs from python, and the plugin handles it. A `BeforeToolCallback` error becomes the model-facing error rather than aborting the run, and the tool still never runs. Two behaviors the Go cell had first now hold on both cells: the after-tool point skips every error path, because the tool-error point already closed the dispatch, and a deferred long-running tool that reaches after-tool with no result is reported as an `indeterminate` outcome. The Go tool-error point sends no second event for a fail-closed error the plugin itself raised.

The Go runtime also serves Foundry-model agents, which the compiler ties to the Go runtime. Deliverable: one image under one tag. kagent resolves `<tag>-full` for agents with skills or `executeCodeBlocks`. The image does not carry that variant, so it does not cover those agents.

The Go plugin classifies spawns by the configured remote-agent names (`Config.SpawnTools`, filled from `AgentConfig.RemoteAgents`). A `transfer_to_agent` call is absent from that list, so it crosses `BeforeToolCallback` as an ordinary `ToolCall` with `spawn:false`. The [VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md) file states this path in prose, and no test exercises it.

An in-process sub-agent scope does not open on the Go cells. There `BeforeAgentCallback` sends `child_start` without a binding. The runtime binds such a start only to a spawn in flight (`in_flight_fork` in [appa-runtime/src/api/session.rs](../../appa-runtime/src/api/session.rs)). With `spawn:false` no fork exists, so the runtime refuses the scope and the plugin fails closed. The rc4 Go `AgentConfig` also has no `sub_agents` field. The go main refuses a config that declares them before the stock decoder runs (`configguard.go`). This path differs from the cross-pod remote agent of the mapping tables, which feeds `ChildStart` and `SpawnResult`.

## Remedy-plan execution

A blocked call answers with feedback that quotes an offer id. The offered plan executes through `execute_remedy_plan`, the reserved MCP tool of the engine. The runtime supplies it, identical for every harness, at `$APPA_RUNTIME_URL/mcp` from process start ([appa-runtime/src/mcp.rs](../../appa-runtime/src/mcp.rs)). The runtime refuses a call no hook vouched for, and executing the act spends the vouch.

Delivery is one more construction delta in each entrypoint. The python entrypoint appends a `McpToolset` over streamable HTTP at `$APPA_RUNTIME_URL/mcp` with a request timeout of 300 s (`REMEDY_CALL_TIMEOUT_SECONDS`). A parked authority consult or a slow sanitizer outlives the ADK default of 5 s. That default fails the remedy execution at the client. So the timeout must exceed the consult budget of the runtime. Those are the same classes kagent-adk itself uses for CRD MCP tools ([v0.9.12 types.py#L223](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/types.py#L223), [rc4 types.py#L224](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/python/packages/kagent-adk/src/kagent/adk/types.py#L224)).

The go main appends an `HttpMcpServerConfig` for `$APPA_RUNTIME_URL/mcp` to the `HttpTools` of the rendered config, with the same 300 s request timeout (`remedyCallTimeoutSeconds`). That is the stock path kagent builds every CRD MCP toolset from ([`tool/mcptoolset`](https://github.com/google/adk-go/tree/v2.1.0/tool/mcptoolset)). The plugin `AppaPluginKagent` answers the `ToolCall` hook of the reserved tool with `PassControl` and lets the call pass. That is the dedicated row in each mapping table above.

Coverage per plan element ([appa-engine/src/plan.rs](../../appa-engine/src/plan.rs)):

| Plan element | On kagent |
|---|---|
| `Authorize(authority)` | Executes engine-side inside the `execute_remedy_plan` call. The engine consults a URL authority then. People out of band answer on their own channel, and a no-answer grants nothing and leaves the offer standing. A `hitl` authority: below. |
| `Accept(narrowing)` | Executes engine-side. The narrowed call redispatches through the normal gate. |
| `Sanitize(sanitizer)` | Executes engine-side, and the sanitized result returns through the mapped after-tool path. |
| `Derive(sanitizer)` | Executes engine-side — the progress hop. |
| `Redispatch` | Needs no id and no reserved tool. The agent calls the named tool, and the normal `ToolCall` gate applies. |
| return declaration | The plans a held spawn offers. Each takes `label`, the lowest label the parent accepts, and the attesting plan takes `return_schema` too. The plugin executes the first plan itself, so no model reads the offer ([Delegation and the fork](#delegation-and-the-fork)). |
| fork advice | Advice, never a remedy. The spawn gate is already mapped. |

### Human review

`appa-runtime` consults its `hitl` authority through MCP elicitation inside the still-open `execute_remedy_plan` call ([appa-runtime/src/elicit.rs](../../appa-runtime/src/elicit.rs)) — the Claude Code channel. The MCP client of a kagent pod carries no elicitation and has no person on it. So on kagent the plugin reaches the person through the kagent approval flow. The ruling returns to the runtime on the wire instead of through the elicitation:

1. The blocking decision carries the review. A `deny_call` lists, per offer whose plan consults a `hitl` authority, the review as the person reads it (`review: [{offer_id, text}]`). That is the same rendering the elicitation shows. It names the authority and its hint, the exact tool, the canonical arguments, and what the ruling covers. The engine builds it at the block ([`pending_reviews` in appa-runtime/src/engine.rs](../../appa-runtime/src/engine.rs)). The session keeps only the `hitl`-backed authorities.
2. The plugin asks through the stock confirmation. `AppaPluginKagent` remembers the reviews. The agent calls `execute_remedy_plan` with a reviewed offer, and no confirmation is on the call yet. Then the plugin requests the ADK tool confirmation, with that text as the hint. It answers the model that it asked the reviewer. The run suspends, and the A2A caller decides. The agent executes every other remedy itself, steered by its instruction and the chat. Those are accept the narrowing, a sanitizer, a human-less authority, and a redispatch. No confirmation gate sits on the reserved tool.
3. The ruling rides the resumed control call. ADK re-runs the call with `tool_confirmation` set, for an approval and for a rejection alike. So the plugin `tool_call` crosses with `ruling: approve` or `ruling: deny` ([appa-adapter-kagent](../../appa-adapter-kagent/src/lib.rs) parses it into `HookEvent::ToolCall { ruling }`). The runtime records it with the vouch. It spends it as the answer of the `hitl` authority for that one execution (`Backend::Hitl` in [appa-runtime/src/external.rs](../../appa-runtime/src/external.rs)). Approve is an approval and the call runs. Reject is a denial and retires every offer naming this authority for this exact call. The kagent flow offers no "cancel". So no-answer arises only when the caller abandons the task, and then the offer stands.

The answer never reaches the model. It crosses as the ADK confirmation and the wire field of the plugin. What the person sees depends on the caller. Over A2A the confirmation request carries the review verbatim in `toolConfirmation.hint`. So an A2A client (a chat bot, an upstream agent) shows the consult artifact and nothing the model said.

The v0.9.12 kagent dashboard renders the pending tool call and its arguments — `execute_remedy_plan` with the offer id. It does not show the hint. Presenting the artifact in the dashboard is an upstream UI change. Verified by hand on kind, in the dashboard and over A2A. An approval runs the restart, and a rejection leaves it blocked. Eight of the other cases assert that no card appears ([e2e/ui](e2e/ui/), [e2e/a2a](e2e/a2a/)).

```text
human review on kagent — the person rules before the act,
and the runtime spends the ruling inside it

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

The go plugin carries the channel the same way. There adk-go hands its `BeforeToolCallback` the tool context. So a reviewed control call raises the confirmation (`RequestConfirmation`, the review text as the hint), and the resumed call reads `ToolConfirmation()` into `ruling`. The plugin `BeforeModelCallback` strips the `adk_request_confirmation` parts from the view of the model, as the kagent approval gate does. See [plugin.go](appa-kagent-adk-go/plugin.go) and the tests in `plugin_test.go`.

One more step belongs to the go runtime main. In adk-go the reviewed call yields its pending response before the confirmation event. Python yields it after, and its executor never converts it. So the `reviewShapedExecutor` of the main drops that one response part from the A2A task. The dashboard renders the approval card only for a call without a response ([VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md)).

## Annotators

A `[[tool]]` entry either declares the complete contract or names a registered annotator, which answers it per proposed call. The consult runs engine-side inside the `ToolCall` round-trip. The envelope carries only the annotator declaration and the artifact ([appa-runtime/src/consult.rs](../../appa-runtime/src/consult.rs)). The kagent wire already supplies the artifact ingredients: the tool name and the arguments as the plugin serializes them.

So annotators need no kagent surface, no plugin change, and no wire change. A no-answer renders as `Refuse` on the `ToolCall` hook, never as model-facing feedback ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs)). That is the mapped `Refuse` leg in each table above.

Three kagent-specific notes:

- **Timeout budget.** The plugin `/hook` client timeout must exceed the runtime consult budget. Otherwise a slow annotator — endpoint, command, or model builtin — becomes a spurious fail-closed block. ADK has no callback timeout, so kagent carries no fail-open hazard. A slow consult costs latency, bounded upstream by A2A client patience.
- **Tool naming and coverage.** An annotation pins to the canonical digest under the tool name as ADK dispatches it. The `[[tool]]` entries and mandates must match that spelling for every toolset. Two spellings, verified on a live cluster: an MCP tool crosses under its plain name. An agent tool crosses as `<namespace>__NS__<agent name>` with underscores. The wildcard entry (`name = "*"`) is the recommended first posture for a fleet. CRD-declared toolsets produce a long tail the policy never names in advance. No tooling generates `[[tool]]` entries. Write them from the `Agent` and `RemoteMCPServer` resources by hand. The reserved `execute_remedy_plan` needs no entry — the runtime recognizes its own tool first.
- **Builtin provisioning.** Model-builtin annotators execute in the `appa-runtime` deployment ([appa-runtime/src/external.rs](../../appa-runtime/src/external.rs)). A `builtin = "llm"` entry needs `[externals.llm]` and model egress from the runtime pod. A `builtin = "claude-code"` entry needs the claude CLI where the runtime runs. The dedicated runtime release owns those dependencies.

## Wire and codec

Each plugin emits one JSON event per gated callback, with the five exceptions the python section names. The first user message of a run emits two events: the session start or the child start, then the prompt. A child scope emits `child_end` at each stop of the child. Four moments emit none. Those are the reviewed first pass of the control call, the deny or review dict at after-tool, the Go error path at after-tool, and both Go error callbacks after a failed-closed call gate. The event carries the kind, trajectory ids, tool name, the arguments, outcome, and value fields. That is the data the matching `HookEvent` variant needs.

The `appa-adapter-kagent` Rust crate parses this wire into `HookEvent`. It renders each `HookDecision` into the response the plugins enforce, through the `Codec` contract of `appa-runtime-api` (`parse`, `render`). The wire carries no policy meaning. The arguments cross as the plugin serializes them from the parsed call ADK hands it (`json=` in python, `json.Marshal` in Go). The codec passes that serialization through unparsed, and the Engine canonicalizes it. The reserved tool crosses as spelled too — `execute_remedy_plan` — so the runtime recognizes its own tool and binds the vouch.

Three fields carry what the plugin routes without the model. A `deny_call` carries `offers`: every remedy the block offers, in the order the feedback lists them. A plan that declares a return of a child also carries `returns`, either `"as_spoken"` or `{"sanitizer": …}`. The plugin reads that field to recognize a held spawn ([Delegation and the fork](#delegation-and-the-fork)). A `deny_call` carries `review` too: the offers whose plans consult a `hitl` authority, with the text the person reads. A `tool_call` of the reserved tool may carry `ruling`, the answer of the person ([Human review](#human-review)). The plugin obtained it through the kagent confirmation. The value is `approve` or `deny`. Any other spelling counts as malformed, and the codec refuses the event.

The fixture rows `tool_call_reserved` and `tool_call_reserved_ruled` in [fixtures/wire-events.jsonl](fixtures/wire-events.jsonl) pin both shapes. The rows `child_end_returned` and `child_end_void` pin the stop of a child, with a value and without one. The python plugin spells the reserved name once, `RESERVED_TOOL` in `wire.py`. One wire and one codec serve both runtime images.

### Wire obligations

The runtime enforces four orderings at event admission. Every driver of this wire — a plugin, a test harness, a replay tool — keeps them, or the runtime refuses it:

1. **Audience narrowing is deny-then-accept, never allow-then-narrow.** A proposal whose `delta.audience` would narrow the trajectory answers `deny_call` and offers to accept the narrowing. An ops-only read into a public trajectory is one. The call proceeds only after `execute_remedy_plan` accepts it and the agent proposes the call again, through the normal gate. Scenario scripts expect that two-step, not a plain allow.
2. **The vouch precedes `/mcp`.** The runtime recognizes `execute_remedy_plan` before the session runs. See `is_control_tool` in [appa-runtime/src/api/session.rs](../../appa-runtime/src/api/session.rs), applied in [appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs). A `tool_call` that quotes an offer this trajectory pursues answers `pass_control` and binds the vouch. A `tool_call` that quotes an offer another trajectory pursues, or no live offer, answers `deny_call` with `[appa] this offer no longer stands; re-propose the call`. A `tools/call` on `/mcp` with no vouched `tool_call` before it meets the refusal `no live offer with this id exists` ([appa-runtime/src/mcp.rs](../../appa-runtime/src/mcp.rs)). The plugins send that `tool_call` from the before-tool point, so the order holds on the runtime images by construction. A driver that bypasses the plugin sends it itself. The after-tool point reports a `tool_result` for the control tool, and the runtime absorbs it with `ack` — no dispatch was opened.
3. **One call at a time, and every allowed call is closed.** An `allow_call` opens a dispatch. Before its `tool_result` (or `spawn_result`) arrives, the runtime answers every further `tool_call` on that trajectory with `deny_call` and the feedback `[appa] a call is already outstanding; propose one call at a time`. The `CallOutstanding` error lives in [appa-runtime/src/api/mod.rs](../../appa-runtime/src/api/mod.rs), raised at admission in [appa-runtime/src/api/session.rs](../../appa-runtime/src/api/session.rs). Python ADK runs parallel function calls concurrently, so `AppaPluginKagent` queues each complete call-to-result lifecycle per `(root, child)` branch. Other branches stay concurrent. Runner close releases an abandoned local lease; the next prompt lets the runtime close the old dispatch. The plugins close each ordinary dispatch from the after-tool and tool-error points, success and failure alike. Both plugins report a deferred result as `indeterminate`. Recovery stays inside the turn. A `turn_end`, or the first `tool_call` after a prompt, closes a dispatch the harness never reported, with the outcome `Indeterminate`. An outcome reported after that close answers `block` with `no open dispatch with this id exists`. So a driver reports each outcome before it proposes the next call, and ends the turn it abandons.
4. **A spawn declares the return of its child before it releases, and the child returns at its own stop.** Under `context_control` the runtime marks an agent-tool proposal a spawn and answers `deny_call` with the return-declaration menu. The call releases only after `execute_remedy_plan` takes one plan with a `label`, and after the driver proposes the same call again. The value of the child then crosses at its own `child_end`, and nowhere else. The `spawn_result` of the parent may carry only the value that crossed. The runtime replays that value, and it withholds any other. A driver that ends a child pod itself, without a stop, delivers nothing to the parent ([Delegation and the fork](#delegation-and-the-fork)).

### Labels and flow completeness

The contract triple — `delta`, `requires`, `emits` — is engine algebra, and no label crosses the wire in either direction. The engine narrows the trajectory label with `delta` when it admits a result. It checks `requires` — membership, `history`, and `attention` marks — against trajectory state at dispatch ([appa-engine/src/check.rs](../../appa-engine/src/check.rs)). It records `emits` into the effect ledger, and effects commit on `Success`, never on `Indeterminate`.

That algebra is sound only over the flows the runtime saw, so the runtime image keeps one invariant. Every value that enters model attention or leaves the agent crosses a mapped hook. If it cannot, an entrypoint wrapper routes it through one hook, or the entrypoint refuses the config. On kagent the list is closed:

- User input crosses at `Prompt`, before the session append.
- Tool results cross at `ToolResult`. A return of a child crosses at `ChildEnd`, at the stop of that child. The `SpawnResult` of the parent replays it or withholds it.
- Delegated entries cross at `ChildStart`, which answers with the return contract the child works under.
- The memory tools `load_memory` and `save_memory`, and artifact loaders, are ordinary tools, so they cross the tool gate. The memory prefetch of a memory agent enters attention without a hook, on both runtimes ([Known gaps](#known-gaps-and-handling)).
- Code execution and the memory write-back cross the tool gate through their entrypoint wrappers ([Out-of-band flows](#out-of-band-flows)).
- The entrypoint constrains the compaction summarizer to the agent model, and refuses any other out-of-band feature.
- The CRD-compiled instruction is static config, not a flow.

Two boundaries stay non-gated by design, and the invariant names them rather than hiding them. The agent reply leaves through the A2A event queue, which only `on_event_callback` sees, and that callback is a liveness gate. The `TurnEnd` event gates nothing, and the implemented model defines no emission event ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs)). The compaction summary re-enters attention without a hook. The liveness gates hold everything else when the `/hook` channel is down.

### Delegation and the fork

A child trajectory starts at the label of its parent — trust and audience. Attention is never trajectory state ([appa-engine/src/plan.rs](../../appa-engine/src/plan.rs): "a child begins at the same label, so a fork cures no requirement").

Under `context_control` the runtime marks an agent-tool proposal a spawn, and it holds the call until the parent declares the return. The block carries the menu. Its first plan takes the return as spoken, and each plan after it names one registered untagged `tool_output` sanitizer, in registry order. Every plan takes `label`, the lowest label the parent accepts from the return.

The plugin routes that declaration itself, so the model never reads the block. On a `deny_call` whose `offers` carry a return route, the plugin does four things:

1. It takes the first offer, the bare floor.
2. It posts a `tool_call` for `execute_remedy_plan` with `{offer_id, label: {}}`. The runtime answers `pass_control` and vouches for the act.
3. It calls `execute_remedy_plan` on `$APPA_RUNTIME_URL/mcp` with those arguments.
4. It posts the identical spawn `tool_call` again, and the runtime releases it.

An empty `label` means the label the parent holds now, the bare floor. The child narrows no further than the parent stands. A return therefore crosses only when it narrows the parent nothing. The plugin takes no policy judgment. It never picks a sanitizer, and neither image asks the model to pick one ([Known gaps](#known-gaps-and-handling)). The model reads one ordinary tool call and its result. Under a bare floor the runtime answers a stop with `ack`, or with the reason. A `child_return` needs a sanitized or attested route, which no kagent agent declares, and both plugins enforce it anyway.

The hooks map the moments of the fork:

| Moment | Hook | Runtime |
|---|---|---|
| The parent proposes the agent tool | `ToolCall{spawn: true}` | Marks the spawn and holds it. The `deny_call` carries the return-declaration menu, and the plugin answers it. |
| The parent declares the return | `ToolCall` on `execute_remedy_plan`, then the `/mcp` call | Vouches for the plan, then executes it. The release prepares the fork under the declared return policy. Its seed is the label of the parent at release. The `AllowCall` carries a `spawn_binding` the kagent plugin does not need: the child binds to the one spawn in flight. |
| First event of the child pod | `ChildStart` (kagent lineage headers in session state) | Opens the fork for the child. It binds the pod to it, and each `ToolCall`/`ToolResult` after that lands in the child trajectory. A `Context` answer carries the return contract. The plugin prepends that text to the first user message of the child. A repeated start of the same pair resumes the child. |
| The child stops | `ChildEnd` | Judges the final message of the child under the return policy of the fork. An `Ack` means it crossed as spoken. An absent value is a void return. A `ChildReturn` names the bytes that cross, so the child posts a second `child_end` carrying them. A `Block` means nothing crossed, and the child stops again with what does ([appa-runtime/src/hooks.rs](../../appa-runtime/src/hooks.rs), `return_decision`). |
| The run of the child ends | `TurnEnd` (child) | Closes a dispatch the child left open. |
| The value reaches the parent | `SpawnResult` | Replays or withholds. A value byte-equal to the latest return of the child answers `Ack`, and the parent reads it unchanged. Any other value comes back withheld with `[appa] the subagent ended outside the return check`. A spawn result with no value reports as an ordinary outcome. An unforked or `indeterminate` spawn result answers as an ordinary tool result (`Ack`/`ReplaceOutput`). |

```text
delegation — the parent declares the return at the spawn,
the child returns at its own stop

parent trajectory · cluster-ops     label: trusted · public
  │  tool_call {spawn: true} ─▶ held: declare the return
  │  the plugin takes the bare floor and proposes again
  │  execute_remedy_plan(offer_id, label: {}) ─▶ the fork
  ▼                                              prepares
child trajectory · log-analyst      label: trusted · public
  │  child_start opens the fork     ◀ inherited
  │  context ─▶ the return contract rides the child's
  │             first user message
  │  get_pod_logs — suspicious ingress: its own gate,
  │  its own remedy, in its own trajectory
  ▼
child label narrows                 label: suspicious · public
  │  child_end ─▶ the child's OWN stop rules the return
  ▼
inside the declaration ─▶ crosses · the child stops with it
a sanitized derivation ─▶ the child returns those bytes,
                          then stops with them (a route the
                          images never declare)
outside it             ─▶ blocked at the child with the
                          reason · it stops again
no value               ─▶ a void return · nothing crosses
  │
  ▼  spawn_result ─▶ replays what crossed, or withholds it
parent receives the crossed value · its label unchanged
```

On the go cells the stock executor does not land the lineage headers in session state. The session-service decorator of the runtime main lands them from the A2A call context on every `Get` and `Create`. So a delegated child classifies as a child there too, from the same headers ([VERIFICATION.md](appa-kagent-adk-go/VERIFICATION.md)). The go plugin classifies each run from the headers landed before it, and it sends `child_start` on every delegated entry. The kagent go remote-agent tool mints one child context id at construction and sends every delegation into it ([Trajectory identity](#trajectory-identity)). One child session id therefore serves every parent in turn. The child pod opens one trajectory per parent, keyed by the root id of that parent. On v0.9.12 one go parent still delegates into a given child once per parent session. A second delegation prepares a second fork, and a child bound to one fork binds no other. The child resumes under the first fork. The second spawn result of the parent then comes back blocked with `the fork and the child are already bound elsewhere`. The rc4 `isolateSessions` field removes the limit with one context per call. The python cell mints a fresh child context per parent request and has no such limit. The demo delegates `cluster-ops` → `log-analyst` under `context_control = true`. The delegation case in both matrices delegates from two fresh parent sessions in turn and asserts that each parent called the child. The A2A matrix asserts that the delegation answers in one of the three shapes a released spawn takes, and never as a denial. The UI matrix asserts that the child card completed with an output that is neither an undeclared-agent denial nor kagent's own failure text. Both assert that the injected instruction never reaches the operator through the child.

## Trajectory identity

- Root `TrajectoryId`: the ADK session id with a harness prefix, per the `appa-runtime-api` convention.
- Child classification: the plugin in the child pod reads the lineage headers the kagent executor lands in session state under `headers`. Both plugins take `x-kagent-root-context-id` as the root id, and `x-kagent-parent-context-id` when the root header is absent. On v0.9.12 that is [_agent_executor.py#L541-L544](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L541-L544). On the v1alpha3 lane it is [_agent_executor.py#L212-L214](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L212-L214). A delegated entry feeds `ChildStart`. A plain external entry feeds `SessionStart` and `Prompt`.
- When the child opens, python: the plugin classifies each run from the headers the kagent executor landed before it. It pins that pair for the run. It sends `child_start` on every delegated entry, as the go plugin does. A repeat of the same pair is a resume the runtime records. So a python child under a go parent binds its own fork, and one shared child session serves each parent under its own root. A python parent shares no child session across parents. The kagent python executor builds a fresh runner per A2A request from the root agent factory ([v0.9.12 _agent_executor.py#L128-L137](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L128-L137), [_a2a.py#L111-L112](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L111-L112)). The remote-agent tool it builds mints the child context id at construction ([_remote_a2a_tool.py#L177](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_remote_a2a_tool.py#L177), [#L324](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_remote_a2a_tool.py#L324)).
- When the child opens, go: the plugin classifies each run from the headers the runtime main landed before it. It sends `child_start` on every delegated entry. The kagent go remote-agent tool mints one `sharedContextID` at construction ([rc4 remote_a2a_tool.go#L211](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/tools/remote_a2a_tool.go#L211)). It sends every delegation from its pod into it while `isolate_sessions` is false, the default ([#L152-L164](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/tools/remote_a2a_tool.go#L152-L164), [#L227-L234](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/adk/pkg/tools/remote_a2a_tool.go#L227-L234)). The v0.9.12 tree has no such field, so the v1alpha2 CRD cannot set it on cell A-go. So one child session id serves every parent in turn, and each parent opens the child under its own root id. The runtime binds a start to the fork of that root, so no parent takes the fork of another. A start of a pair the runtime already holds open answers `ack`. It resumes the child under the current label of the parent. On v0.9.12 one go parent delegates into a given child once per parent session. A second delegation prepares a second fork, and the child resumes under the first. The second spawn result of the parent then comes back blocked with `the fork and the child are already bound elsewhere`. The rc4 `isolateSessions` field removes the limit with one context per call. The python cell has no such limit.
- A successful child reply arrives as `{"result": ...}` for Task replies and as a bare string for direct Message replies. The python plugin reads both shapes. The Go plugin reads the result map, because `functiontool` in adk-go normalizes both replies into it.
- Parent and child run as separate workloads (Deployments on lane A/B1, Substrate Actors on B2). So the hooks of one trajectory come from two plugin instances. Both must reach the same `appa-runtime`. A per-pod runtime never sees the spawn of the parent, so it refuses the `child_start` of the child (`SpawnNotTaken`), and the delegation fails closed.

Delegation needs a name. The runtime serves kagent under `SpawnCoverage::Declared` (`appa-runtime/src/api/mod.rs`, the binary picks it per adapter). A `ToolCall{spawn: true}` releases only under a contract written for the wire name of the agent. The wildcard, which covers every ordinary call the policy does not write, covers no spawn.

The runtime denies an unnamed agent before the engine sees the call, and no child ever opens. The model reads `EventError::UndeclaredSpawn` as its feedback. An ordinary call to a tool nothing covers keeps its operational refusal. Only the spawn gets a policy denial the model reads, because the model can act on it by not delegating.

Claude Code keeps `SpawnCoverage::Wildcard`. The packaged quickstart policy names no agent, so the runtime denies every delegation there. A policy that names an agent releases that spawn. The demo policy names the log analysts and deliberately not the release managers. The blocked case in both matrices proves the denial on both cells, and `appa-runtime/tests/kagent_spawns.rs` proves the rule against a wildcard policy.

## Known gaps and handling

| Gap | Lane / runtime | Handling |
|---|---|---|
| No callback at ADK session creation | all | `SessionStart` synthesizes at first invocation, sent before `Prompt`. A never-invoked session emits nothing and flows nothing. |
| No error-turn callback in google-adk 1.31.1 | A-py | Earlier error callbacks plus `Indeterminate` classification at recovery. Closed on the lane B python cells by the google-adk 2.8.0 error callbacks. |
| No error-turn callback in adk/v2 | A-go, B1-go, B2-go | v2.1.0 and v2.2.0 lack run-error and agent-error callbacks. Recovery classifies the open dispatch `Indeterminate`, as on cell A-py. |
| Human review on remedy plans | none | Both plugins carry the human ruling through the stock kagent confirmation ([Human review](#human-review)). |
| Go image name on stable | A-go | v0.9.12 has no Go-image knob. The cluster must pull the image under the name the controller derives from `controller.agentImage` (`…/golang-adk:<tag>`). The release publishes `golang-adk` on the digest of `appa-kagent-adk-go`, so a released version needs nothing more. A source build needs the kind load the chart README shows. |
| Shared child context on a go parent | A-go | The kagent go remote-agent tool sends every delegation from a pod into one child context while `isolate_sessions` is false ([rc4 types.go#L437-L442](https://github.com/kagent-dev/kagent/blob/v0.10.0-rc4/go/api/adk/types.go#L437-L442)). The v0.9.12 CRD cannot set the field. The child ADK session then carries what earlier parents said into the model context of the child. That ingress crosses trajectories, and no hook sees it. The behavior is kagent design. The rc4 field closes it with one context per call. Each parent opens the child under its own root id, so the runtime keeps one trajectory per parent. On v0.9.12 one go parent delegates into a given child once per parent session. The second delegation prepares a second fork, and the child resumes under the first. The second spawn result of the parent then comes back blocked with `the fork and the child are already bound elsewhere`. |
| A spawn declares the bare floor only | all | The plugin answers the return-declaration menu itself. It takes the first plan: the return as spoken, floored at the label the parent holds at the spawn. No kagent agent takes a sanitized or attested route, so a return sanitizer never applies at the stop of a child. The same sanitizers still apply at a confined tool result. A return therefore crosses only when it narrows the parent nothing. |
| An `AgentTool` child binds no fork | A-py, B1-py | The python plugin classifies `AgentTool` as a spawn. Such a child runs in a nested Runner with a fresh session and no lineage headers. It opens its own root trajectory, and its calls cross the gate there. The spawn result of the parent names no child, and the runtime closes it with `the spawn did not take`. A declarative kagent config declares no `AgentTool`: both images refuse `sub_agents`, and `remote_agents` cross as A2A spawns. |
| No `-full` image variant | A-go, B1-py, B1-go | kagent resolves `<tag>-full` for agents with skills or `executeCodeBlocks`: on v0.9.12 for the Go runtime, on v0.10.0-rc4 for either runtime. Neither OpenAPPA image carries that variant. Those agents resolve a name the images do not serve, so the gate does not cover them. |
| CRD in-process `sub_agents` | B2 | Both images refuse the config instead of dropping children. The python entrypoint exits 2. The Go main reads the raw config once and runs the config it decodes from those bytes. It exits 1 on `sub_agents`, `agent_plugins`, or any other top-level key outside the rc4 schema. |
| Out-of-band ADK features (code exec, memory write-back, compaction) | python cells, go cells | Code execution and the memory persist cross the tool gate as `ToolCall`s. The entrypoint constrains the compaction summarizer to the agent model, and refuses what it can neither gate nor constrain. On the go cells the guard refuses `execute_code` true and a `context_config` that is not null with exit 1. See [Out-of-band flows](#out-of-band-flows). |
| Memory prefetch: stored memories enter attention with no hook | all | A `memory` config installs a prefetch that searches the memory on the first turn and appends the hits to the model instructions, with no function call (python `prefetch_memory`, Go `preloadmemorytool`). Neither plugin sees that ingress, and the model callbacks are liveness gates. A memory agent runs with this ingress ungated on both runtimes. |
| Non-gated emission: the agent reply and the compaction summary | all (the summary: python cells) | Named by design. The reply leaves through `on_event` (liveness only) on both runtimes. The summary re-enters attention ungated on the python cells. The implemented model defines no emission event. |
| Pre-gate session-name metadata | all | Stock kagent creates the session before the `Prompt` hook runs, on both runtimes. Python names it with the first 20 characters of the stripped prompt text, plus an ellipsis when longer. Without a text part it leaves the name empty (v0.9.12 `_agent_executor.py:693-705`). Go names it with the text, cut to 20 bytes plus `...` when longer. Without a text part it omits the name (rc4 `adk/pkg/a2a/executor.go:175-201`, `414-424`). The runtime answers every `Prompt` with `Ack` and gates no prompt text, so the name lands in the session store. The write is stock kagent code, so closing the metadata leak needs an upstream change. |
| BYO agents | all | Per-agent images outside any shared runtime image. Their authors add the one plugin line, in either language. |
| Sandbox kinds (`AgentHarness`, `SandboxAgent`) | all | Different subsystem, out of scope. |
| Upstream has no plugin config knob | all | The entrypoints replay stock behavior through public calls. CI pins kagent and ADK versions per lane. The python suite runs on both locked ADKs, and the Go suite runs on the locked adk/v2. |

## Demo chart

The demo composes the dedicated `appa-runtime` chart with the fixture-only `appa-kagent-demo` chart ([demo/chart](demo/chart)) on cells A-py and A-go. kagent 0.9.12 uses `appa-kagent-adk` for enabled declarative Python Agents. Go Agents use the derived `golang-adk` image. An Agent that resolves a `-full` variant stays uncovered.

The `appa-runtime` release is the sole owner of the runtime Deployment and Service, serving policy ConfigMap, persistence, and `appa-guide`. The fixture release owns none of them. It installs:

- Agents `cluster-ops`, `log-analyst`, and `release-manager`, plus optional Go twins. Every Agent receives the explicit shared `APPA_RUNTIME_URL` and an existing `modelConfig.name`. The chart creates no provider Secret or ModelConfig.
- The `demo-tools` Deployment, Service, and `RemoteMCPServer` ([demo/Dockerfile](demo/Dockerfile)).
- A standalone `appa-demo-mocks` Deployment and Service. They implement `runbook-readers`, `release-window`, `change-board`, and both deterministic sanitizers. Fixed Python command adapters in [demo.appa.toml](demo/chart/files/demo.appa.toml) forward consult envelopes from the runtime to this Service. Direct cleartext URL bindings remain loopback-only.
- `ConfigMap/appa-kagent-demo-policy`, an inert rendered policy template. It is neither mounted nor served. `appa-guide` verifies the Helm release, compares the template with serving policy, and applies only the approved merge to the runtime-owned ConfigMap.
- A post-install and post-upgrade seed Job. It replays sixteen captured transcripts through the controller API under deterministic `uuid5` ids.

The policy names delegated children by the wire spelling `<namespace>__NS__<child>`, with hyphens changed to underscores. The chart renders both child names. It refuses collisions among fixed and configurable Agent names. The schema constrains every name to a DNS-1123 label. A changed name updates only the fixture policy template; the operator must approve the corresponding serving-policy change through `appa-guide`.

```text
appa-runtime release
  Deployment + Service appa-runtime:18787
  serving policy + persistence
  appa-guide Agent

appa-kagent-demo release
  cluster-ops fleet ──APPA_RUNTIME_URL──▶ appa-runtime
  demo-tools       Deployment + Service + RemoteMCPServer
  appa-demo-mocks  Deployment + Service
  policy template  ConfigMap ──approved merge through appa-guide
  seed Job         ──▶ kagent-controller
```

## Delivery units

| Unit | Content |
|---|---|
| 1 | `appa-adapter-kagent` Rust codec crate: wire parse to `HookEvent`, decision render, `Adapter` enum variant, unit tests against recorded wire fixtures |
| 2 | `appa-kagent-adk` Python package: `AppaPluginKagent` with the callback table, per-ADK deltas, fail-closed transport, liveness gates, deny-dict self-recognition, `PassControl` pass-through, and the human-review channel. The channel: `review` remembered from `deny_call`, the confirmation request on a reviewed reserved call, `ruling` on the resumed call |
| 3 | Python entrypoint: strict config schema and refusal rules, stock plugin parity, both config deliveries, and the controller args contract. Also the `reasoning_effort` fill from `APPA_KAGENT_OPENAI_REASONING_EFFORT`, and the reserved-tool toolset with its 300 s request timeout |
| 4 | Python OCI image: the kagent app base image pinned by tag and digest, the wheel installed offline with `--no-deps`. The release publishes it with an SBOM and a provenance attestation. |
| 5 | Lane A end-to-end: kind cluster with the stable chart, `controller.agentImage` swap, parent-and-child scenario against one shared runtime. An operator installs the stack by hand (the chart README), and the matrices run against it. |
| 5b | The fixture-only demo Helm chart ([Demo chart](#demo-chart)). It holds the demo tools, standalone mock policy services, six Agents, an inert rendered policy template, and the seeded showcase chats. It references the runtime and ModelConfig owned by their dedicated releases. It owns no runtime, serving policy, persistence, provider Secret, ModelConfig, or `appa-guide`. The Go twins need the derived Go image. The two-chart composition drives both live matrix cells. |
| 6 | `appa-kagent-adk-go`: adk/v2 mapping verification, the Go plugin and runtime main, one image under two published names, the reserved-tool toolset |
| 7 | Lane B end-to-end: the B1 dual-knob swap on the release-candidate chart, and B2 Harness × AgentTemplate on the Substrate path. Not run |

Every unit lives in this repository.

[release.yml](../../.github/workflows/release.yml) publishes five images to `ghcr.io/archestra-ai` at the release version. `appa-runtime` and `appa-kagent-adk` publish for `linux/amd64` and `linux/arm64`. `appa-kagent-adk-go`, `appa-demo-tools`, and `appa-demo-mocks` publish for `linux/amd64`. Each build attaches an SBOM and provenance. The workflow publishes both Helm charts as OCI artifacts and GitHub release assets. It also tags `golang-adk` on the `appa-kagent-adk-go` digest for kagent 0.9.12.

Every Dockerfile pins each `FROM` and `COPY --from` base by digest, with the tag beside it for the reader. The image jobs in [ci.yml](../../.github/workflows/ci.yml) build all five on a pull request that can break them, including native arm64 checks for the runtime and Python adapter. They push none. No workflow scans the images.

## Verification matrix

Adapter tests (per runtime, in `appa-kagent-adk/tests`, `appa-kagent-adk-go/*_test.go` and `appa-kagent-adk-go/cmd/appa-kagent-adk-go/main_test.go`, each row against a scripted `/hook` server):

- Callback-to-event mapping for every table row, the stop of a child at `ChildEnd`, and both child-reply shapes. Spawn classification goes by tool type in python and by configured name in Go. Two python tests pin child classification from either lineage header (`test_a_parent_context_header_alone_classifies_as_the_childs_start`, `test_the_root_header_wins_over_the_parent_header` in `test_plugin.py`). Each plugin pins the opening of a shared child session. Every parent opens the child under its own root, and a root session still opens once at its first content. A repeated delegated entry of an open pair sends no second `child_start`, and each plugin pins that. Each also pins the invocation-level id pin: every callback of one run, the turn end included, carries the pair the run open read (`test_an_opened_invocation_keeps_its_ids_when_the_headers_change_mid_run`, `TestAnOpenedInvocationKeepsItsIdsWhenTheHeadersChangeMidRun`). One python test pins the same pin on the google-adk 2.8.0 error path, where no after-run callback fires (`test_a_run_error_ends_the_turn_under_the_pinned_pair_and_releases_it`).
- Deny path: a `DenyCall` skips execution, reaches the model as the function response, and is not double-reported.
- Replace path: `ReplaceOutput` at the after-tool point, a `ChildReturn` echoed at the stop of the child, and a `Block` as the withheld result at the parent.
- Prompt ordering: a `block` answer raises before the session append (`test_a_blocked_prompt_raises_before_the_append`, `TestABlockedPromptFailsBeforeTheAppend`). The shipped runtime answers every `Prompt` with `Ack`.
- Fail closed: the runtime down at each gated callback blocks the action. The liveness gates hold the model and emission callbacks. The one exception is `turn_end`, a best-effort post that logs and never blocks a finished turn (`test_a_turn_end_reports_and_never_blocks`, `TestATurnEndReportsAndNeverBlocks`). A `refuse` answer at each gated Go callback returns nothing to ADK and posts nothing further. The failure carries the detail of the runtime (`TestARefuseAnswerFailsEveryGatedCallbackClosed` in `plugin_test.go`).
- `PassControl`: the reserved `execute_remedy_plan` call proceeds untouched (per plugin). The runtime refuses an unvouched `/mcp` call in `appa-runtime/src/mcp.rs`, and no kagent test exercises it.
- Human review (per plugin): a reviewed offer raises the confirmation before the control call crosses. The resumed call carries the ruling. The plugin never reports the review dict as a result.
- Out-of-band gate (python): a `DenyCall` on the code-execution `ToolCall` skips the subprocess, and the code output crosses at `ToolResult`. A `DenyCall` on the memory-persist `ToolCall` skips `add_session_to_memory` (`test_gates.py`). On the v0.9.12 lane `test_equivalence.py` drives the wrapped persist callback of a real memory agent. A recording memory service stands in for the kagent one. A denied persist never reaches the service. An allowed one writes once and reports one `tool_result` (`test_a_denied_persist_of_a_real_memory_agent_writes_nothing`, `test_an_allowed_persist_of_a_real_memory_agent_writes_once_and_reports_once`).
- Compaction constraint (python): the entrypoint accepts a `summarizer_model` equal to the agent model, and refuses a divergent one at startup.
- Startup refusal (python): unknown config fields, `sub_agents`, and a divergent compaction `summarizer_model` (`test_entrypoint.py`, `test_config_guard.py`). On every lane `test_config_guard.py` pins that the bare name of an aliased field is unknown unless the class validates by name (`test_the_bare_name_of_an_aliased_field_counts_only_under_validate_by_name`). It pins that a key pydantic reads through an `AliasPath` is unknown (`test_a_key_read_through_an_alias_path_is_refused_as_unknown`). On the v0.9.12 lane it pins the refusal of a sibling-variant key with its path (`test_a_key_of_a_sibling_model_variant_is_refused_with_its_path`). It pins that both TLS spellings of a model pass (`test_both_tls_spellings_on_the_model_pass_and_reach_the_field`). It pins that the three TLS keys inside `http_tools[].params` and `sse_tools[].params` pass and reach the tool config (`test_the_tls_keys_inside_mcp_params_pass_and_reach_the_tool_config`). The v0.9.12 controller renders the TLS settings of a `RemoteMCPServer` there, and kagent lifts them to the tool config. The python `main()` returns 2 without `APPA_RUNTIME_URL` and on a refused config, with the diagnostic on stderr (`test_main_returns_2_without_a_runtime_url`, `test_main_returns_2_on_a_refused_config` in `test_entrypoint.py`). It returns 2 on a config that does not validate, with one line on stderr and no traceback and no value (`test_main_returns_2_on_a_config_that_does_not_validate`). The python plugin constructor refuses an empty runtime URL.
- Startup refusal (Go): the main refuses to start without `APPA_RUNTIME_URL` (`TestTheRuntimeRefusesToStartUngated`). Its config guard refuses `sub_agents`, `agent_plugins`, any top-level key outside the rc4 schema, and a document that is not one JSON object. It accepts a nested unknown key, because the stock decoder drops it. On the decoded config it refuses `execute_code` true and a `context_config` that is not null, and it accepts every `network` shape. A key refusal wins over a value refusal. A config the stock decoder cannot decode surfaces as the decoder's own error. An accepted config equals what the stock decoder decodes from the same bytes (`TestTheConfigGuardRefusesWhatThisImageCannotRunAsDeclared`). The built binary exits 1 with the key on stderr for a mounted `config.json`, a `KAGENT_CONFIG_JSON` delivery, and a `-filepath` dir. It exits 1 on `execute_code` true inside the binary. An accepted config passes the stock validation and reaches the agent card load, the one stock load left on disk (`TestTheRuntimeRefusesToStartOnAnUnsupportedConfig` in `main_test.go`). The nested build of that binary runs under a three-minute timeout, and a missing go tool fails the test outside `-short`. The test `TestTheAcceptedTopLevelKeysAreExactlyTheRC4Schema` pins the thirteen accepted keys, so a kagent module bump lands with a decision.
- Args contract: both mains accept the controller args and answer readiness at the stock endpoint. No adapter test exercises it. The live matrices run the pods the controller starts.
- No link from the codec crate to `appa-runtime` or `appa-engine` (`appa-adapter-kagent/Cargo.toml` depends on `appa-runtime-api` only), and no policy state in either plugin.

Per-version checks:

- CI runs the python suite on both locked ADKs ([.github/workflows/ci.yml](../../.github/workflows/ci.yml)). The `uv.lock` lane resolves google-adk 2.8.0. The v0.9.12 lane adds kagent-adk 0.3.0 and google-adk 1.31.1 with `--with`. On each, `test_the_installed_plugin_manager_accepts_every_callback` runs the real `PluginManager` through `before_tool_callback` with the keyword names the plugin declares.
- On the v0.9.12 lane `test_equivalence.py` pins stock parity. The gated startup builds the agent `kagent.adk.cli.static` builds, with the same tool names, instruction and model dump. The reserved toolset comes last among the tools, and `AppaPluginKagent` last among the plugins (`test_the_gated_startup_builds_the_stock_agent_and_appends_the_plugin_last`). The file `tool_names.v0.9.12.json` records the built-in tools the stock builder attaches: `ask_user`, and with a memory config `prefetch_memory`, `load_memory` and `save_memory`. The test regenerates the record, and a real runner proposes each declared name through the gate under that spelling. The undeclared `prefetch_memory` crosses as a failure result under its spelling (`test_the_recorded_tool_names_are_what_the_stock_builder_attaches`, `test_a_proposal_of_each_recorded_name_crosses_the_gate_as_spelled`). The locked lane skips the file.
- The Go suite pins the adk/v2 callback surface with `TestTheADKPluginSurfaceCarriesEveryCallback`, and CI runs `go vet` and `gofmt` beside it.
- The shared fixture [fixtures/wire-events.jsonl](fixtures/wire-events.jsonl) pins the wire. The Rust codec, the python plugin, and the Go plugin all read it.

Every kagent job in [.github/workflows/ci.yml](../../.github/workflows/ci.yml) runs only on its own inputs. A first job, `changes`, reads the changed paths with `dorny/paths-filter` and answers five questions: `python`, `go`, `chart`, `integration` and `e2e`. Each kagent job takes one answer as its condition. A commit outside a job's inputs runs none of it. The Rust workspace jobs stay unconditional, because they cover the codec crate and the shared fixtures.

The integration suite ([tests/](tests/)) drives the real gated path with no cluster, no dashboard, no model and no API key. One pytest session starts a real `appa runtime --adapter kagent` on a copy of the demo policy, the real demo MCP tools, and the real mock externals. It builds a parent and a delegated child through `appa_kagent_adk.entrypoint.build_server`, and serves each over kagent's A2A endpoint on its own loopback port. The suite scripts the model alone, so every decision is the runtime's. It runs behind `APPA_INTEGRATION=1`, and it skips without the kagent v0.9.12 lane.

| File | Cases | Substance |
|---|---|---|
| [test_core.py](tests/test_core.py) | 6 | the ordinary read, the refused exfiltration, the sanitized default, a forged offer id, the gated crash logs and the gated third-party status page |
| [test_remedies.py](tests/test_remedies.py) | 11 | both chat steers, the human-review authority both ways, the annotator on a public and an ops runbook, the release window in and out of window, the change board approving, denying and staying silent |
| [test_delegation.py](tests/test_delegation.py) | 5 | the value of a child crossing at its own stop and the parent replaying it, the declared floor binding the reads of the child, a child that returns nothing and then says more, a delegation the policy never names, two parents in one child session |

The job `kagent-integration` runs those twenty-two cases on the kagent v0.9.12 lane, against a debug `appa` binary it builds. It gates every pull request that touches the python package, the suite, the demo tools, the mocks, the demo policy, or the runtime crates. It uploads the trajectory database and every process log on a failure.

The job `kagent-e2e-subset` runs all eighteen A2A cases against a real model, on a kind cluster it stands up itself. It runs on cell A-py alone, on the `run-e2e` label or a manual dispatch, and never on a fork pull request, which cannot read the OpenRouter key. Each test gets three total attempts, and exhaustion fails the job. The model matches the public playground: `openai/gpt-5.6-luna` at OpenRouter.

Three scripts under [e2e/ci/](e2e/ci/) load the images, install kagent plus both OpenAPPA charts, and run the cases. A person runs the same three on a laptop or a dev VM. A placeholder job of the same name names the label on every other update.

The live matrices span three dimensions, and every combination is a row. The dimensions are kagent version, runtime plugin, and driver. The python plugin runs against the google-adk that kagent version locks. The go plugin runs on the adk/v2 v2.1.0 inside its image. The driver is the dashboard in headless Chromium, or A2A `message/send` alone. Each row runs the same eighteen conversations from [e2e/ui](e2e/ui/) and [e2e/a2a](e2e/a2a/). The index is [e2e/README.md](e2e/README.md), and the runner is `e2e/run-matrix.sh`.

Local matrices assume a running stack at `APPA_UI_URL` and `APPA_A2A_URL`. The scripts under [e2e/ci/](e2e/ci/) provision the A-py A2A row. All four kagent v0.9.12 rows pass after the child-return and remote-runtime changes. The PR CI runs the Go unit suite and integration suite. A labeled pull request adds the complete eighteen-case A-py A2A row. The v0.10 rows have no stack.

| kagent | Cell | Runtime plugin | Driver | Status |
|---|---|---|---|---|
| v0.9.12 | A-py | python · google-adk 1.31.1 | dashboard | 18/18 |
| v0.9.12 | A-py | python · google-adk 1.31.1 | A2A | 18/18 |
| v0.9.12 | A-go | go · adk/v2 v2.1.0 | dashboard | 18/18 |
| v0.9.12 | A-go | go · adk/v2 v2.1.0 | A2A | 18/18 |
| v0.9.12 | A-py | python · google-adk 1.31.1 | A2A, all 18 cases in CI | gates after three attempts per case, on the `run-e2e` label |
| v0.10.0-rc4 | B1-py | python · google-adk 2.8.0 | dashboard, A2A | not run |
| v0.10.0-rc4 | B1-go | go · adk/v2 v2.1.0 | dashboard, A2A | not run |
| main | B2-py, B2-go | python · google-adk 2.8.0, go · adk/v2 v2.1.0 (kagent main locks v2.2.0) | dashboard, A2A | not run |

End-to-end tests, each with its status — `[automated: <suite>]`, `[unit-level]`, `[verified by hand on kind]`, or `[not run]`:

- Lane A: declarative python agent on a kind cluster with the stable chart and the `controller.agentImage` swap. Gated tool calls, replaced results, denied delegations. Manual steps (the chart README): the kind install, the kagent install, and the image swap. [verified by hand on kind]
- Lane B1: both image knobs swapped on the release-candidate chart. A python agent and a go agent gated side by side. [not run]
- Lane B2: `AgentTemplate` × `Harness` on the Substrate path — admission by selector, `KAGENT_CONFIG_JSON` delivery, the env-var cap respected. [not run]
- Cross-workload trajectory: parent and delegated child against one shared runtime. The delegation case in both matrices delegates from two fresh parent sessions in turn and asserts that each parent called the child. The A2A matrix asserts that the delegation answers in one of the three shapes a released spawn takes, and never as a denial. The UI matrix asserts that the child card completed with an output that is neither an undeclared-agent denial nor kagent's own failure text. Both assert that the injected instruction never reaches the operator [verified by hand on kind]. Each plugin pins the `ChildStart`, `ChildEnd` and `SpawnResult` pieces, and `appa-runtime/tests/kagent_spawns.rs` pins the spawn `ToolCall` gate [unit-level]. The file `appa-runtime/tests/kagent_returns.rs` pins the whole return contract on the wire: the held menu, the declaration, the crossing at `child_end`, the echo of a sanitized return, the schema block of an attested one, the replay at the parent, and the withhold of bytes that never crossed [automated: `cargo test -p appa --test kagent_returns`]. No test asserts that both land in one trajectory log [not run].
- Remedy execution per plan element: accept-narrowing and sanitize run in the matrices [verified by hand on kind]. Those are the exfiltration ask, the configured default, the accept and no-remedy steers. Authorize with an external Authority runs both ways [verified by hand on kind]. Human-less: release-window, in and out of window. People out of band: the change board, a parked consult ruled through its own channel (approve, deny, unanswered). The derive hop, redispatch, and the vouch spent once per act have no test [not run].
- Human review: the plugin raises the kagent confirmation on the reviewed `execute_remedy_plan` call [verified by hand on kind]. An approval from the caller re-runs the call with `ruling: approve`, and the act executes. A rejection re-runs it with `ruling: deny`, the authority denies, and the runtime retires the offer. Each plugin also pins the channel [unit-level].
- Annotated tool: the consult happens once and rules per call [verified by hand on kind]. The annotation pins to the canonical digest in the fact (`appa-engine/src/fact.rs`), and no kagent test replays it [not run].
- Annotator down: the gated call refuses at the `ToolCall` hook, and nothing model-facing crosses. [automated: `appa-runtime/tests/annotators.rs`, `every_annotation_failure_refuses_the_hook_and_appends_nothing`, runtime-level]
- Wildcard: a tool the policy never names routes through the wildcard annotator and runs annotated. [automated: `appa-runtime/tests/annotators.rs`, `the_wildcard_annotates_an_unwritten_tool_and_an_exact_declaration_never_consults`, runtime-level]
- Crash window: kill the agent workload between `ToolCall` and `ToolResult`, then make sure the runtime reports the dispatch `Indeterminate`. No test kills an agent workload on the kagent wire. The test `appa-runtime/tests/crash_recovery.rs` kills the runtime on the Claude Code wire and asserts the dispatch stays open. [not run]
- Error-turn window per cell: on cell A-py and every go cell, force an unhandled model failure. Then make sure recovery closes the turn at the next `turn_end`, or at the first `tool_call` after the next prompt. On the lane B python cells, make sure the error callbacks post the `turn_end`s. Each plugin pins the error callbacks (`test_the_error_turns_report_quietly`, `TestThePluginsOwnGateErrorIsNotReportedAsAToolFailure`, `TestTheErrorPathDoesNotDoubleReportAtTheAfterToolPoint`) [unit-level]. No cluster run forces the failure [not run].
- Out-of-band flows on cell A-py: a code-execution agent whose policy denies the code sees the subprocess skipped. A memory agent whose policy denies the persist writes nothing to the memory backend. Pinned in `test_gates.py` with fake executors and callbacks [unit-level]. No cluster run exercises either [not run].
- Integration suite ([tests/](tests/)): twenty-two of the matrix cases on the real gated path, with a scripted model and no cluster. The real runtime, the real tools, the real mock externals, and both agents built by the real entrypoint. [automated: `integrations/kagent/tests` under `APPA_INTEGRATION=1`, a pull-request gate]
- Live matrices on the Helm-installed stack ([e2e/ui](e2e/ui/), [e2e/a2a](e2e/a2a/)): eighteen cases each, the same conversations with a real model. One runs through the dashboard in headless Chromium, and the other over A2A `message/send` alone. Both answer the `oncall` review both ways. Both play the change-board member on the mock side channel (approve, deny, unanswered). Nine cases assert that no confirmation card appears. Those are the exfiltration ask, the configured default, the accept steer, and the no-remedy steer. The other five are the forged offer id, the in-window release-window approval, both delegations, and the change-board approval. The other six non-review cases assert nothing about cards. The A2A driver waits `APPA_A2A_DECISION_SETTLE` (2 s) before it answers a confirmation. The reason: kagent persists the confirmation-request event while it answers the request. The variables `APPA_AGENT` (UI) and `APPA_A2A_URL` (A2A) select the go twin for either matrix. 18/18 on cell A-py and 18/18 on cell A-go after the child-return and remote-runtime changes [verified by hand on kind]. The three steer-dependent cases carry two reruns, because the model sometimes picks another remedy. Those are the configured default, the accept steer, and the no-remedy steer. Either way the test asserts what the gate did, off the tool results.
