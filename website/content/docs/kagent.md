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

[kagent](https://github.com/kagent-dev/kagent) runs LLM agents on Kubernetes. The operator creates `Agent` resources, and the kagent controller runs each declarative agent on a shared runtime image that the install configuration selects. This proposal gates those agents with OpenAPPA through that stock configuration. It does not fork, patch, or vendor kagent or Google ADK.

`appa-adapter-kagent` is the adapter. Its image extends kagent's published agent-runtime image with two files: a small entrypoint and one Google ADK plugin. The plugin maps ADK callbacks to the eight `appa-runtime` `/hook` events and enforces the returned decisions inside the ADK dispatch loop. `appa-runtime` stays a separate process. It owns policy, the Engine, consults, remedy plans, trajectory state, and `appa.db`. Policy semantics stay in [How it works](/how-it-works) and [Policy contracts](/contracts).

Two stock surfaces carry the whole integration:

- The helm value `controller.agentImage` selects the runtime image for every declarative agent. Naming the adapter image there is ordinary install configuration.
- `KAgentApp(plugins=[...])` is a public constructor parameter of kagent's published runtime library. kagent registers its own plugins through the same parameter, and the adapter plugin registers beside them.

The [implementation plan](../../../integrations/kagent/IMPLEMENTATION.md) pins the exact source baselines and backs every claim on this page with code evidence.

## Overview

- The platform operator points `controller.agentImage` at the adapter image. Every declarative python-runtime agent — the default runtime — rolls onto it. No CRD edits, no agent changes.
- `APPA_RUNTIME_URL` arrives as a baked image default, or per agent through the agent's deployment env.
- Inside each agent pod, the adapter entrypoint rebuilds the compiled agent from the mounted config — the same steps as the stock entrypoint — and registers `AppaHookPlugin`.
- Each gated ADK callback becomes one `/hook` request to the shared `appa-runtime`. The plugin enforces the returned `HookDecision` where the callback fires: it can deny a tool call with feedback the model reads, replace a tool result, or substitute a child's return.
- Hooks fail closed. When the runtime is unreachable or answers outside the contract, the gated action does not run.

## Highlights

### Gated agents on Kubernetes

```text
kagent controller — stock, unmodified
  watches the operator's Agent resources
  runtime image = helm controller.agentImage  ◀── the knob
        │
        │  renders per Agent:
        │  Deployment + Service + config Secret
        ▼
┌─ agent pod · one per Agent ───────────────────────────┐
│                                                       │
│  image    controller.agentImage = appa-adapter-kagent │
│  /config  the compiled agent, mounted as data:        │
│           config.json + agent-card.json               │
│                                                       │
│  entrypoint ─▶ KAgentApp(plugins=[ ..stock..,         │
│                AppaHookPlugin ]) ─▶ serve A2A         │
└──────────────────────────┬────────────────────────────┘
                           │  POST /hook · fail closed
                           ▼
┌─ appa-runtime · one shared service ───────────────────┐
│  policy · Engine · consults · remedy plans ·          │
│  trajectory state · appa.db                           │
└───────────────────────────────────────────────────────┘
```

The agent exists in the pod only as mounted configuration. No developer code and no per-agent image exists there, so one generic adapter image serves every declarative agent.

All gated pods of one deployment report to one shared `appa-runtime`. A parent and each agent it calls run as separate workloads, and their hooks must reach the same runtime to correlate into one trajectory.

### One image wraps the stock runtime

```text
┌─ appa-adapter-kagent image ───────────────────────────┐
│                                                       │
│  OpenAPPA layer — two files                           │
│    entrypoint.py        replays the stock entrypoint  │
│                         steps and accepts the same    │
│                         args the controller sends     │
│    appa_hook_plugin.py  ADK BasePlugin ─▶ POST /hook  │
│                                                       │
├─ base: kagent's published runtime image · unmodified ─┤
│    kagent runtime lib   its CLI present, not PID 1    │
│    google-adk           BasePlugin is its official    │
│                         plugin API                    │
└───────────────────────────────────────────────────────┘

entrypoint flow — the stock calls, one delta:

  cfg = AgentConfig.model_validate(config.json)
                             # refuse unknown fields
  plugins  = [ ..stock plugins.. ]
  plugins += [ AppaHookPlugin(APPA_RUNTIME_URL) ]  ◀ delta
  KAgentApp(root_agent, card, url, name,
            plugins=plugins).build()   ─▶ serve A2A
```

The stock entrypoint performs the identical sequence with a closed plugin list. The plugin list handed to ADK becomes its plugin manager, so one registration covers the root agent, every sub-agent, and every tool. Google ADK stays an unmodified dependency, and the image keeps the stock runtime contract: the same args, the same serving port, the same readiness endpoint.

### Callback-to-hook mapping

The runtime's hook vocabulary is the eight `HookEvent` variants of `appa-runtime-api`. The plugin maps each gated ADK callback onto exactly one event. Callbacks with no event either pass through or hold as liveness gates.

```text
ADK plugin callback                     ─▶ feeds /hook event
time flows top→bottom within one turn   ◀  decisions answered
─────────────────────────────────────────────────────────────

session   first invocation of a fresh ADK session,
│         detected in on_user_message_callback
│         (no callback exists at session creation)
│           ─▶ [1] SessionStart   ◀ Ack
│              root TrajectoryId = the ADK session id
▼
prompt    on_user_message_callback — fires BEFORE the
│         session append, so a Block keeps the exact
│         bytes out of stored history
│           ═▶ [2] Prompt         ◀ Ack | Block
│         before_run_callback · before_agent_cb (root)
│           ─x no event: Prompt gates the same bytes
▼
model     before_model · after_model · on_model_error
│           ─x no event: held as liveness gates —
│              /hook channel down ⇒ refuse
▼
tool      before_tool_callback — a deny dict SKIPS
loop      execution and becomes the function response
│         the model reads
│           ═▶ [3] ToolCall{spawn:false}
│              ◀ AllowCall | DenyCall | Refuse
│         after_tool_callback — a returned dict
│         replaces what the model sees
│           ═▶ [4] ToolResult
│              ◀ Ack | ReplaceOutput | Block
│         on_tool_error_callback
│           ─▶ [4] ToolResult{Failure}
▼
child     before_tool_cb on the agent tool
deleg.    │ ═▶ [3] ToolCall{spawn:true}
│         │    ◀ AllowCall{binding}
│         │ child scope opens, in its own pod:
│         │   the child plugin classifies the
│         │   delegated entry
│         │     ─▶ [5] ChildStart        ◀ Ack
│         │   … the child runs its own [3]/[4] loop …
│         │   child-side after_run_callback
│         │     ─▶ [6] TurnEnd (child)   ◀ Ack
│         └ after_tool_cb on the agent-tool return —
│           the ONE point where the value the parent
│           receives can be substituted
│             ═▶ [7] SpawnResult
│                ◀ Ack | ChildReturn | ReplaceOutput
│                  | Block
▼
emit      on_event_callback
│           ─x no event: held as a liveness gate
▼
turn      after_run_callback — fires on normal
end       completion and after a pre-run halt; the
          pinned google-adk has no error-turn callback
          (fail-closed rule 4)
            ─▶ [6] TurnEnd (root)  ◀ Ack
               closes tool dispatches the turn abandoned

          [8] ChildEnd — unfed BY DESIGN: return
              substitution is enforceable only on the
              parent side, so returns cross at
              [7] SpawnResult. The Claude Code adapter
              makes the same choice.
```

The enforcement comes from ADK's own plugin contract, verified in the pinned google-adk sources:

- A dict returned from `before_tool_callback` skips execution and becomes the function response the model reads — `DenyCall` with feedback. The deny dict also flows through `after_tool_callback`, so the plugin recognizes its own deny payload and does not report it twice.
- A non-None return from `after_tool_callback` replaces the result the model sees — `ReplaceOutput`.
- `on_user_message_callback` fires before the runner appends the message to session history, so a `Block` on `Prompt` is a pre-append barrier.
- An agent declares another agent as a tool, and kagent dispatches it as an ordinary ADK tool — so the tool-call gate is also the spawn gate.

Each called agent runs in its own pod with its own plugin instance. The child side classifies the delegated entry and feeds `ChildStart` and the child `TurnEnd`. The parent side feeds the spawn `ToolCall` and `SpawnResult`. Both report to the one shared runtime.

### Fail-closed rules

1. An unreachable `/hook` endpoint, or a response outside the contract, blocks the gated action. The plugin raises, and ADK aborts the invocation.
2. A mounted config with a field the entrypoint does not support refuses to start, and the pod stays unready. The stock config parser ignores unknown fields — the adapter must not inherit that silence.
3. The model and emission callbacks feed no event, but they still hold the action when the `/hook` channel is down.
4. The pinned google-adk defines no error-turn callback, so a turn that dies on an unhandled error emits no `TurnEnd`. The model-error and tool-error callbacks catch the common failures earlier. For the rest, `appa-runtime` recovery classifies the open dispatch as `Indeterminate` at the next admitted event, and the next `Prompt` fails closed if the runtime is down.

### Scope and limits

- Covered: declarative agents on the python runtime — the default — in the current stable kagent release.
- Not covered: go-runtime agents (opt-in, and the Go ADK's plugin list is compiled in), BYO agents (per-agent images whose authors add the one plugin line themselves), and kagent's sandbox kinds.
- `SessionStart` is a first-invocation proxy. A session that is created but never invoked emits nothing, and also flows nothing.
- The entrypoint replays the stock entrypoint's behavior instead of calling it, because upstream has no plugin configuration knob. Each upstream release therefore costs one small equivalence re-check. A one-field upstream contribution would remove the duplication.
- Forward path: kagent's release-candidate line replaces the Agent controller with a `Harness` × `AgentTemplate` model, where the same adapter image lands in the Harness's required workload-image field. The implementation plan covers both lanes in full.

## Implementation plan

The [kagent implementation plan](../../../integrations/kagent/IMPLEMENTATION.md) pins the source baselines, defines the artifacts, the entrypoint and plugin specification, the runtime-side codec, both delivery lanes with their rollout procedures, trajectory identity, and the verification matrix — with code evidence for every claim.
