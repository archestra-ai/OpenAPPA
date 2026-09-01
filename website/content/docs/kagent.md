---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Proposal for the OpenAPPA kagent adapter — an ADK plugin delivered through kagent's agent-runtime image setting, with no kagent or Google ADK fork.
---

:::proposal
name: kAgent
date: 2026-09-01
:::

[kagent](https://github.com/kagent-dev/kagent) runs LLM agents on Kubernetes. Its public API is `kagent.dev/v1alpha2`: the operator creates `Agent` resources (type `Declarative` or `BYO`), and the controller runs each Declarative agent as a Deployment on a shared runtime image. This proposal gates those agents with OpenAPPA through stock configuration. It does not fork, patch, or vendor kagent or Google ADK.

`appa-adapter-kagent` is the adapter. Its image extends the published `kagent-adk` runtime image with two files: a small entrypoint and one Google ADK plugin. The plugin maps ADK callbacks to the eight `appa-runtime` `/hook` events and enforces the returned decisions inside the ADK dispatch loop. `appa-runtime` stays a separate process. It owns policy, the Engine, consults, remedy plans, trajectory state, and `appa.db`. Policy semantics stay in [How it works](/how-it-works) and [Policy contracts](/contracts).

Two stock surfaces carry the whole integration:

- Helm value `controller.agentImage.{registry,repository,tag}` selects the runtime image for every Declarative agent ([values.yaml#L179-L183](https://github.com/kagent-dev/kagent/blob/v0.9.12/helm/kagent/values.yaml#L179-L183) → controller ConfigMap `IMAGE_*` env ([controller-configmap.yaml#L12-L18](https://github.com/kagent-dev/kagent/blob/v0.9.12/helm/kagent/templates/controller-configmap.yaml#L12-L18)) → controller `--image-*` flags). Naming the adapter image there is ordinary install configuration.
- `KAgentApp(plugins=[...])` is a public constructor parameter of the published `kagent-adk` package ([_a2a.py#L63](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L63)). kagent registers its own plugins through the same parameter ([cli.py#L69-L79](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L69-L79)).

Source baseline — the last stable release: kagent [`v0.9.12`](https://github.com/kagent-dev/kagent/releases/tag/v0.9.12) (2026-07-20, the API the public docs describe), kagent-adk 0.3.0, google-adk 1.31.1 (the workspace lock). Every enforcement claim below is re-verified against the google-adk 1.31.1 wheel, not carried over from newer versions.

## Overview

- The platform operator sets three helm values: `controller.agentImage.{registry,repository,tag}` name the adapter image. Every Declarative `runtime: python` agent — and `python` is the CRD default ([agent_types.go#L175](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L175)) — rolls onto it. No CRD edits, no agent changes.
- `APPA_RUNTIME_URL` arrives as a baked image default, or per agent via `spec.declarative.deployment.env` ([agent_types.go#L443-L445](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/api/v1alpha2/agent_types.go#L443-L445)).
- Inside each agent pod, the adapter entrypoint rebuilds the compiled agent from the Secret-mounted `/config` — the same steps as stock `kagent-adk static` — and registers `AppaHookPlugin`.
- Each gated ADK callback becomes one `/hook` request to the shared `appa-runtime`. The plugin enforces the returned `HookDecision` where the callback fires: it can deny a tool call with feedback the model reads, replace a tool result, or substitute a child's return.
- Hooks fail closed. When the runtime is unreachable or answers outside the contract, the gated action does not run.

## Highlights

### Gated agents on Kubernetes

```text
Kubernetes cluster
+---------------------------------------------------------------------------+
| kagent controller (stock, v0.9.12)                                        |
|   watches kagent.dev/v1alpha2 Agent resources                             |
|   runtime image for Declarative agents = helm controller.agentImage       |
|   renders per agent: Deployment + Service + config Secret                 |
+------------------------------------+--------------------------------------+
                                     | one Deployment per Agent
                                     v
+---------------------- agent pod (one per Agent) --------------------------+
| image: controller.agentImage = appa-adapter-kagent                        |
| /config (Secret): config.json (the compiled agent), agent-card.json      |
| args: --host <bind> --port 8080 --filepath /config                        |
|                                                                           |
| adapter entrypoint -> KAgentApp(plugins=[.., AppaHookPlugin]) -> A2A :8080|
+-----------------------------+---------------------------------------------+
                              | POST /hook  (fail closed)
                              v
+------------------- appa-runtime (one shared service) ---------------------+
| policy, Engine, consults, remedy plans, trajectory state, appa.db         |
+---------------------------------------------------------------------------+
```

The controller writes the compiled agent into a per-agent Secret mounted at `/config` ([manifest_builder.go#L243](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L243)) and passes `--host/--port/--filepath` as container args ([deployments.go#L175-L179](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/deployments.go#L175-L179)). The agent exists in the pod only as that data. No developer code and no per-agent image exists there, so one generic adapter image serves every Declarative agent.

All gated pods of one deployment report to one shared `appa-runtime`. A parent and each agent it calls run as separate workloads, and their hooks must reach the same runtime to correlate into one trajectory.

### One image wraps kagent-adk

```text
appa-adapter-kagent image
+-------------------------------------------------------------+
| OpenAPPA layer (two files)                                  |
|   entrypoint.py       replays the `kagent-adk static` steps |
|                       and accepts the same --host/--port/   |
|                       --filepath args the controller sends  |
|   appa_hook_plugin.py ADK BasePlugin -> POST /hook          |
+-------------------------------------------------------------+
| base: published kagent-adk (kagent/app) image, unmodified   |
|   kagent-adk 0.3.0    (cli.py present, not used as PID 1)   |
|   google-adk 1.31.1   (BasePlugin is its official API)      |
+-------------------------------------------------------------+

entrypoint flow — the same public calls as stock `static`, one delta:
  cfg = AgentConfig.model_validate(/config/config.json)  # refuse unknown fields
  plugins = [<stock STS / passthrough plugins>]
  plugins.append(AppaHookPlugin(APPA_RUNTIME_URL))       # <-- the delta
  KAgentApp(root_agent, card, url, name,
            plugins=plugins).build()                     # serve A2A on :8080
```

Stock `static` performs the identical sequence with a closed plugin list ([cli.py#L54-L101](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/cli.py#L54-L101)). The plugin list handed to ADK becomes its plugin manager, so one registration covers the root agent, every sub-agent, and every tool. The image must keep the stock runtime contract: serve A2A on port 8080 and answer readiness at `/.well-known/agent-card.json` ([manifest_builder.go#L532](https://github.com/kagent-dev/kagent/blob/v0.9.12/go/core/internal/controller/translator/agent/manifest_builder.go#L532)). Google ADK stays an unmodified dependency.

### Callback-to-hook mapping

The runtime's hook vocabulary is the eight `HookEvent` variants in `appa-runtime-api/src/lib.rs`. The plugin maps each gated ADK callback onto exactly one event. google-adk 1.31.1 defines 12 plugin callbacks (`google/adk/plugins/base_plugin.py`, lines 114-348 in the wheel). Callbacks with no event either pass through or hold as liveness gates.

```text
   ADK CALLBACK (google-adk 1.31.1, unmodified)          APPA RUNTIME /hook — the 8 HookEvents
   time flows top→bottom within one turn                 and the decisions each one answers
   ─────────────────────────────────────────             ─────────────────────────────────────

session  first invocation of a fresh ADK session ──────▶ [1] SessionStart      ◀ Ack
  │        detected in on_user_message_callback;             root TrajectoryId derived
  ▼        no callback exists at session creation            from the ADK session id
prompt   on_user_message_callback ─────────────────────▶ [2] Prompt            ◀ Ack | Block
  │        fires BEFORE the session append
  │        (runners.py 1537 then 1550), so a Block
  │        keeps the exact bytes out of history
  │      before_run_callback ───────x no event  (Prompt already gates the same bytes)
  │      before_agent_cb (root) ────x no event  (root entry IS Prompt)
  ▼
model    before_model_callback ─────x no event ┐ no model-layer HookEvent exists;
         after_model_callback ──────x no event │ the plugin holds these callbacks as
         on_model_error_callback ───x no event ┘ liveness gates: /hook down ⇒ refuse
  │
  ▼
tool     before_tool_callback ═════════════════════════▶ [3] ToolCall{spawn:F} ◀ AllowCall | DenyCall
loop        deny: the returned dict SKIPS execution                              | Refuse
  │         and becomes the function response the
  │         model reads (functions.py 509-534)
  │      after_tool_callback ══════════════════════════▶ [4] ToolResult        ◀ Ack | ReplaceOutput
  │      on_tool_error_callback ───────────────────────▶ [4] ToolResult{Failure} | Block
  ▼
child    before_tool_cb (agent tool) ──────────────────▶ [3] ToolCall{spawn:T} ◀ AllowCall{binding}
deleg.     │ child scope opens (its own pod):
           │  child-side plugin classifies the
           │  delegated entry ─────────────────────────▶ [5] ChildStart        ◀ Ack
           │      … child runs its own [3]/[4] loop …
           │  child-side after_run_callback ───────────▶ [6] TurnEnd (child)   ◀ Ack
           └ after_tool_cb (agent tool return) ────────▶ [7] SpawnResult       ◀ Ack | ChildReturn{value}
               the ONE point where the value the                                 | ReplaceOutput | Block
               parent receives can be substituted
  │
  ▼
emit     on_event_callback ─────────x no event  (no emission HookEvent;
  │                                              held as a liveness gate)
  ▼
turn     after_run_callback ───────────────────────────▶ [6] TurnEnd (root)    ◀ Ack —
end        fires on normal completion and after a          closes tool dispatches
           before_run halt; google-adk 1.31.1 has NO       the turn abandoned
           error-turn callback — see fail-closed rule 4

                                                         [8] ChildEnd — unfed BY DESIGN:
                                                             return substitution is enforceable
                                                             only parent-side, so returns cross
                                                             at [7] SpawnResult. The Claude Code
                                                             adapter makes the same choice.
```

The enforcement mechanics come from ADK's own plugin contract, re-verified in the 1.31.1 wheel:

- A dict returned from `before_tool_callback` skips execution and becomes the function response the model reads — `DenyCall` with feedback (`functions.py`, lines 509-534 and 588-592). The deny dict also flows through `after_tool_callback`, so the plugin recognizes its own deny payload and does not report it twice.
- A non-None return from `after_tool_callback` replaces the result the model sees — `ReplaceOutput` (`functions.py`, lines 547-576).
- `on_user_message_callback` fires before the runner appends the message to session history (`runners.py`, lines 1537-1556), so a `Block` on `Prompt` is a pre-append barrier.
- A v1alpha2 agent declares another agent as a tool (`spec.declarative.tools[].type: Agent`), and kagent dispatches it as an ordinary ADK tool, `KAgentRemoteA2ATool` ([_remote_a2a_tool.py#L158-L170](https://github.com/kagent-dev/kagent/blob/v0.9.12/python/packages/kagent-adk/src/kagent/adk/_remote_a2a_tool.py#L158-L170)) — so the tool-call gate is also the spawn gate.

Each called agent runs in its own pod with its own plugin instance. The child side classifies the delegated entry and feeds `ChildStart` and the child `TurnEnd`. The parent side feeds the spawn `ToolCall` and `SpawnResult`. Both report to the one shared runtime.

### Fail-closed rules

1. An unreachable `/hook` endpoint, or a response outside the contract, blocks the gated action. The plugin raises, ADK wraps the exception, and the invocation aborts (`plugin_manager.py`, lines 288-305 in the wheel).
2. A `/config/config.json` with a field the entrypoint does not support refuses to start, and the pod stays unready. Stock `AgentConfig` ignores unknown fields — the adapter must not inherit that silence.
3. The model and emission callbacks feed no event, but they still hold the action when the `/hook` channel is down.
4. google-adk 1.31.1 has no error-turn callback (`on_run_error_callback` and `on_agent_error_callback` do not exist in this version), so a turn that dies on an unhandled error emits no `TurnEnd`. `on_model_error_callback` and `on_tool_error_callback` catch the common failures earlier. For the rest, `appa-runtime` recovery classifies the open dispatch as `Indeterminate` at the next admitted event, and the next `Prompt` fails closed if the runtime is down.

### Scope and limits

- Covered: `kagent.dev/v1alpha2` `Agent` resources with `type: Declarative` and `runtime: python` — the CRD default — on stable kagent v0.9.12.
- Not covered: `runtime: go` agents (opt-in — v0.9.12 has no helm value for the Go runtime image, and the Go ADK's plugin list is compiled in), BYO agents (per-agent images whose authors add the one plugin line themselves), and the `AgentHarness`/`SandboxAgent` sandbox kinds.
- `SessionStart` is a first-invocation proxy. A session that is created but never invoked emits nothing, and also flows nothing.
- The entrypoint replays the behavior of `kagent-adk static` instead of calling it, because upstream has no plugin configuration knob. Each `kagent-adk` release therefore costs one small equivalence re-check. A one-field upstream contribution would remove the duplication.
- Forward path: kagent's main branch is mid-cutover to a `v1alpha3` `AgentTemplate` × `Harness` model on Substrate Actors, unreleased and publicly undocumented. The 0.10 release candidates also flip the runtime default to `go` and add a `controller.goAgentImage` value. The same adapter image moves to `Harness.spec.workload.image` there. The [implementation plan](../../../integrations/kagent/IMPLEMENTATION.md) carries the details.

## Implementation plan

The [kagent implementation plan](../../../integrations/kagent/IMPLEMENTATION.md) defines the artifacts, the entrypoint and plugin specification, the runtime-side codec, deployment and rollout, trajectory identity, the forward (v1alpha3) lane, and the verification matrix.
