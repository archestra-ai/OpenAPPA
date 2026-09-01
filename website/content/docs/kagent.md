---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: Proposal for OpenAPPA on kagent — an ADK plugin delivered through kagent's runtime-image settings, with no kagent or Google ADK fork.
---

:::proposal
name: kAgent
date: 2026-09-01
:::

[kagent](https://github.com/kagent-dev/kagent) runs LLM agents on Kubernetes. This proposal gates every declarative kagent agent with OpenAPPA through one install setting: the runtime image. No kagent fork, no Google ADK fork, no agent changes.

Two stock surfaces carry the whole integration:

- kagent's runtime-image settings name the image that runs every declarative agent. Point them at the OpenAPPA images: `appa-kagent-adk` for the python runtime, `appa-kagent-adk-go` for the Go runtime.
- Both runtimes take plugins through Google ADK's official plugin API. The OpenAPPA images register one — `AppaHookPlugin` — which maps ADK callbacks to the eight `appa-runtime` hook events and enforces the answered `HookDecision`.

`appa-runtime` owns the decisions: policy, the Engine, remedy plans, and trajectory state, as [How it works](/how-it-works) and [Policy contracts](/contracts) define them.

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
│  image    controller.agentImage = appa-kagent-adk     │
│  /config  the compiled agent, mounted as data:        │
│           config.json + agent-card.json               │
│                                                       │
│  entrypoint ─▶ KAgentApp(plugins=[ ..stock..,         │
│                AppaHookPlugin ]) ─▶ serve A2A         │
└──────────────────────────┬────────────────────────────┘
                           │  POST /hook · fail closed
                           ▼
┌─ appa-runtime ────────────────────────────────────────┐
│  policy · Engine · consults · remedy plans ·          │
│  trajectory state · appa.db                           │
└───────────────────────────────────────────────────────┘
```

The agent exists in the pod only as mounted configuration. One generic image therefore serves every declarative agent, and the rollout is one install-setting change.

### One image per runtime wraps the stock runtime

```text
┌─ appa-kagent-adk image ───────────────────────────────┐
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

The entrypoint replays the stock startup and appends one plugin to the list ADK already accepts. That one registration covers the root agent, every sub-agent, and every tool. The Go image does the same through the Go ADK's plugin API.

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
              [7] SpawnResult.
```

The load-bearing enforcement points, proven in the pinned ADK sources:

- A deny returned from the before-tool callback skips execution and becomes the function response the model reads.
- The user-message callback fires before the session append, so a blocked prompt never lands in stored history.
- An agent called as a tool crosses the same gate as any tool call, and its return is substituted on the parent side.

### Fail-closed rules

1. An unreachable `/hook`, or an answer outside the contract, blocks the gated action.
2. A config field the entrypoint does not support refuses to start. The stock parser's silence is not inherited.
3. The model and emission callbacks feed no event, but they still hold when the `/hook` channel is down.
4. When the pinned ADK has no error-turn callback, `appa-runtime` recovery closes the turn at the next admitted event.

### Scope

Covered: declarative agents on both runtimes. Not covered: BYO agents and kagent's sandbox kinds.

## Implementation plan

The [kagent implementation plan](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) carries the rest: source baselines, the target matrix, per-version mapping tables, both delivery lanes, the quickstart option, and the verification matrix. Code evidence backs every claim there.
