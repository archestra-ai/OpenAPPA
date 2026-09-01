---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Proposal for the OpenAPPA kagent adapter — an ADK plugin delivered in the Harness workload image, with no kagent or Google ADK fork.
---

:::proposal
name: kAgent
date: 2026-09-01
:::

[kagent](https://github.com/kagent-dev/kagent) runs LLM agents on Kubernetes. Its controller compiles each declarative agent (an `AgentTemplate` resource) into configuration, and runs it on a runtime image that the operator names in a `Harness` resource. This proposal gates kagent agents with OpenAPPA through those stock surfaces. It does not fork, patch, or vendor kagent or Google ADK.

`appa-adapter-kagent` is the adapter. Its workload image extends the published `kagent-adk` image with two files: a small entrypoint and one Google ADK plugin. The plugin maps ADK callbacks to the eight `appa-runtime` `/hook` events and enforces the returned decisions inside the ADK dispatch loop. `appa-runtime` stays a separate process. It owns policy, the Engine, consults, remedy plans, trajectory state, and `appa.db`. Policy semantics stay in [How it works](/how-it-works) and [Policy contracts](/contracts).

Two upstream surfaces carry the whole integration:

- `Harness.spec.workload.image` is a required, operator-supplied, digest-pinned image reference ([harness_types.go#L34-L40](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/api/v1alpha3/harness_types.go#L34-L40)). Every kagent install names a runtime image there. Naming the adapter image is ordinary configuration.
- `KAgentApp(plugins=[...])` is a public constructor parameter of the published `kagent-adk` package ([_a2a.py#L65](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L65)). kagent registers its own plugins through the same parameter ([cli.py#L95-L105](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/cli.py#L95-L105)).

Source baseline: kagent commit [`52cc4de2`](https://github.com/kagent-dev/kagent/commit/52cc4de2a044a5062d10c4f189d863937c1bb0f9) (2026-09-01), google-adk 2.8.0 (kagent's lockfile resolution), Substrate v0.0.20 ([go.mod#L489](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/go.mod#L489)).

## Overview

- The platform operator edits one `Harness`: point `spec.workload.image` at the adapter image, and set `APPA_RUNTIME_URL` in `spec.env`.
- The Harness label selector chooses which `AgentTemplate` resources run gated ([harness_types.go#L114-L117](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/api/v1alpha3/harness_types.go#L114-L117), [collections.go#L85-L102](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/controller/collections.go#L85-L102)). Agent developers change nothing.
- Inside each agent Actor, the adapter entrypoint rebuilds the compiled agent from `KAGENT_CONFIG_JSON` — the same steps as stock `kagent-adk static` — and registers `AppaHookPlugin`.
- Each gated ADK callback becomes one `/hook` request to `appa-runtime`. The plugin enforces the returned `HookDecision` where the callback fires: it can deny a tool call with feedback the model reads, replace a tool result, or substitute a child's return.
- Hooks fail closed. When the runtime is unreachable or answers outside the contract, the gated action does not run.

## Highlights

### Gated agents on Kubernetes

```text
Kubernetes cluster
+---------------------------------------------------------------------------+
| kagent controller-v2 (stock)                                              |
|   watches AgentTemplate, Harness, ModelConfig, RemoteMCPServer            |
|   pairs   Harness.spec.allowedAgentTemplates selector -> AgentTemplates   |
|   renders one immutable Revision per (AgentTemplate, Harness) pair        |
+------------------------------------+--------------------------------------+
                                     | runs each Revision as a Substrate Actor
                                     v
+--------------------- Substrate Actor (one per agent) ---------------------+
| image: Harness.spec.workload.image = appa-adapter-kagent (digest-pinned)  |
| env:   KAGENT_CONFIG_JSON (the compiled agent), APPA_RUNTIME_URL, ...     |
|                                                                           |
| adapter entrypoint -> KAgentApp(plugins=[.., AppaHookPlugin]) -> A2A :8080|
+-----------------------------+---------------------------------------------+
                              | POST /hook  (fail closed)
                              v
+------------------- appa-runtime (one shared service) ---------------------+
| policy, Engine, consults, remedy plans, trajectory state, appa.db         |
+---------------------------------------------------------------------------+
```

The controller injects the compiled agent as environment variables into the Actor ([actor_template.go#L43-L44](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/core/v2/substrate/actor_template.go#L43-L44)). The agent exists in the pod only as that data. No developer code and no per-agent image exists there, so one generic adapter image serves every admitted `AgentTemplate`.

All gated Actors of one deployment report to one shared `appa-runtime`. A parent and its delegated children run in separate Actors, and their hooks must reach the same runtime to correlate into one trajectory.

### One image wraps kagent-adk

```text
appa-adapter-kagent image
+-------------------------------------------------------------+
| OpenAPPA layer (two files)                                  |
|   entrypoint.py       replays the `kagent-adk static` steps |
|   appa_hook_plugin.py ADK BasePlugin -> POST /hook          |
+-------------------------------------------------------------+
| base: published kagent-adk image, unmodified                |
|   kagent-adk package  (cli.py present, not used as PID 1)   |
|   google-adk 2.8.0    (BasePlugin is its official API)      |
+-------------------------------------------------------------+

entrypoint flow — the same public calls as stock `static`, one delta:
  materialize_from_env("/config")                  # env -> config files
  cfg = AgentConfig.model_validate(config)         # refuse unsupported fields
  plugins = [<stock STS / passthrough plugins>]
  plugins.append(AppaHookPlugin(APPA_RUNTIME_URL)) # <-- the delta
  KAgentApp(lambda: cfg.to_agent(name), card, url, name,
            plugins=plugins).build()
```

Stock `kagent-adk static` performs the identical sequence with a closed plugin list ([cli.py#L76-L135](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/cli.py#L76-L135)). The steps are materialize ([_config_materialize.py#L55-L69](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_config_materialize.py#L55-L69)), then validate and rebuild the agent ([types.py#L387-L403](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/types.py#L387-L403)), then serve ([_a2a.py#L126-L149](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_a2a.py#L126-L149)). The plugin list handed to ADK's `App(plugins=...)` becomes ADK's `PluginManager`, so one registration covers the root agent, every sub-agent, and every tool. Google ADK stays an unmodified dependency.

### Callback-to-hook mapping

The runtime's hook vocabulary is the eight `HookEvent` variants in `appa-runtime-api/src/lib.rs`. The plugin maps each gated ADK callback onto exactly one event, and callbacks with no event either pass through or hold as liveness gates.

```text
   ADK CALLBACK (google-adk 2.8.0, unmodified)           APPA RUNTIME /hook — the 8 HookEvents
   time flows top→bottom within one turn                 and the decisions each one answers
   ─────────────────────────────────────────             ─────────────────────────────────────

session  first invocation of a fresh ADK session ──────▶ [1] SessionStart      ◀ Ack
  │        detected in on_user_message_callback;             root TrajectoryId derived
  ▼        no callback exists at session creation            from the ADK session id
prompt   on_user_message_callback ─────────────────────▶ [2] Prompt            ◀ Ack | Block
  │        fires BEFORE the session append, so a
  │        Block keeps the exact bytes out of
  │        stored session history
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
  │         model reads
  │      after_tool_callback ══════════════════════════▶ [4] ToolResult        ◀ Ack | ReplaceOutput
  │      on_tool_error_callback ───────────────────────▶ [4] ToolResult{Failure} | Block
  ▼
child    before_tool_cb (sub-agent tool) ──────────────▶ [3] ToolCall{spawn:T} ◀ AllowCall{binding}
deleg.     │ child scope opens:
           │  before_agent_cb (child) ─────────────────▶ [5] ChildStart        ◀ Ack
           │      … child runs its own [3]/[4] loop …
           │  after_agent_cb (child) ──────────────────▶ [6] TurnEnd (child)   ◀ Ack
           └ after_tool_cb (sub-agent return) ─────────▶ [7] SpawnResult       ◀ Ack | ChildReturn{value}
               the ONE point where the value the                                 | ReplaceOutput | Block
               parent receives can be substituted
  │
  ▼
emit     on_event_callback ─────────x no event  (no emission HookEvent;
  │                                              held as a liveness gate)
  ▼
turn     after_run_callback ───────────────────────────▶ [6] TurnEnd (root)    ◀ Ack —
end      on_run_error_callback ────────────────────────▶ [6] TurnEnd (root,      closes tool dispatches
         on_agent_error_cb (child) ────────────────────▶ [6] TurnEnd  failure)   the turn abandoned

                                                         [8] ChildEnd — unfed BY DESIGN:
                                                             return substitution is enforceable
                                                             only parent-side, so returns cross
                                                             at [7] SpawnResult. The Claude Code
                                                             adapter makes the same choice.
```

The enforcement mechanics come from ADK's own plugin contract, verified in the 2.8.0 wheel:

- All 14 callbacks above exist on `BasePlugin` (`google/adk/plugins/base_plugin.py`, lines 114-394 in the 2.8.0 wheel).
- A dict returned from `before_tool_callback` skips execution and becomes the function response the model reads — `DenyCall` with feedback (`google/adk/flows/llm_flows/functions.py`, lines 611-641).
- A dict returned from `after_tool_callback` replaces the result the model sees — `ReplaceOutput` (`functions.py`, lines 652-683).
- `on_user_message_callback` fires before the runner appends the message to session history (`google/adk/runners.py`, lines 675-700), so a `Block` on `Prompt` is a pre-append barrier.
- kagent dispatches every declared remote sub-agent as an ordinary ADK tool, `KAgentRemoteA2AToolset` ([types.py#L521](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/types.py#L521)), so the tool-call gate is also the spawn gate.

kagent children run in separate Actors. The child Actor's own plugin recognizes the delegated entry from kagent's inbound metadata ([_agent_executor.py#L212-L214](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/_agent_executor.py#L212-L214)). It feeds `ChildStart` and the child `TurnEnd`, while the parent's plugin feeds the spawn `ToolCall` and `SpawnResult`. Both report to the one shared runtime.

### Fail-closed rules

1. An unreachable `/hook` endpoint, or a response outside the contract, blocks the gated action. The plugin raises, and ADK aborts the invocation.
2. A rendered config with a field the entrypoint does not support refuses to start, and the Actor stays unready. In-process `sub_agents` compiled from CRD sub-agent tools are the known case: stock `kagent-adk` drops them silently, and the adapter refuses instead.
3. The model and emission callbacks feed no event, but they still hold the action when the `/hook` channel is down.
4. A hard Actor crash emits nothing. `appa-runtime` recovery classifies the open dispatch as `Indeterminate` at the next admitted event.

### Scope and limits

- Covered: declarative `AgentTemplate` agents on the kagent (Python ADK) harness variant, run from this workload image on the Substrate path (helm `controller.substrate.enabled`).
- Not covered: the Claude harness, which omits hooks, permission mode, memory, and inline MCP from its supported contract ([config.go#L48-L50](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/go/harness/claude/config/config.go#L48-L50)).
- Also not covered: the Codex harness, the non-ADK framework wrappers, and agents on kagent's stock image, whose plugin list is closed ([cli.py#L95-L105](https://github.com/kagent-dev/kagent/blob/52cc4de2a044a5062d10c4f189d863937c1bb0f9/python/packages/kagent-adk/src/kagent/adk/cli.py#L95-L105)).
- `SessionStart` is a first-invocation proxy. A session that is created but never invoked emits nothing, and also flows nothing.
- The entrypoint replays the behavior of `kagent-adk static` instead of calling it, because upstream has no plugin configuration knob. Each `kagent-adk` release therefore costs one small equivalence re-check. A one-field upstream contribution would remove the duplication.

## Implementation plan

The [kagent implementation plan](../../../integrations/kagent/IMPLEMENTATION.md) defines the artifacts, the entrypoint and plugin specification, the runtime-side codec, deployment profiles, trajectory identity, and the verification matrix.
