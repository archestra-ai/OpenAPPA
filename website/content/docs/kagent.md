---
title: kAgent
nav_title: kAgent
category: Integrations
order: 6
description: OpenAPPA on kagent — gate every declarative agent through one runtime-image setting.
---

:::proposal
name: kAgent
date: 2026-09-01
:::

[kagent](https://github.com/kagent-dev/kagent) runs LLM agents on Kubernetes. This proposal gates every declarative kagent agent with OpenAPPA through one install setting: the runtime image.

Two stock surfaces carry the whole integration:

- The kagent runtime-image settings name the image that runs every declarative agent. Point them at the OpenAPPA images: `appa-kagent-adk` for the python runtime, `appa-kagent-adk-go` for the Go runtime.
- Both runtimes take plugins through the official Google ADK plugin API. The OpenAPPA images register one — `AppaPluginKagent` — which maps ADK callbacks to the eight `appa-runtime` hook events and enforces the answered `HookDecision`.

`appa-runtime` owns the decisions: policy, the Engine, remedy plans, and trajectory state. [How it works](/how-it-works) and [Policy contracts](/contracts) define them.

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
│                AppaPluginKagent ]) ─▶ serve A2A       │
└──────────────────────────┬────────────────────────────┘
                           │  POST /hook · fail closed
                           │  /mcp · execute_remedy_plan
                           ▼
┌─ appa-runtime ────────────────────────────────────────┐
│  policy · Engine · consults · remedy plans ·          │
│  trajectory state · appa.db                           │
│  binds loopback only: a shared runtime sits behind a  │
│  relay that rewrites Host (the demo chart)            │
└───────────────────────────────────────────────────────┘
```

The agent exists in the pod only as mounted configuration. One generic image therefore serves every declarative agent, and the rollout is one install-setting change.

### One image per runtime wraps the stock runtime

```text
┌─ appa-kagent-adk image ───────────────────────────────┐
│                                                       │
│  OpenAPPA layer — one package, appa_kagent_adk        │
│    entrypoint.py  replays the stock entrypoint steps  │
│                   and accepts the same args the       │
│                   controller sends                    │
│    plugin.py      ADK BasePlugin ─▶ POST /hook        │
│    wire.py        the wire: events, decisions, the    │
│                   reserved tool's name                │
│                                                       │
├─ base: kagent's published runtime image · unmodified ─┤
│    kagent runtime lib   its CLI present, not PID 1    │
│    google-adk           BasePlugin is its official    │
│                         plugin API                    │
└───────────────────────────────────────────────────────┘

entrypoint flow — the stock calls, five deltas:

  cfg = AgentConfig.model_validate(config.json)
                     # refuse unknown fields         ◀ delta
  cfg.model.reasoning_effort ??=
      $APPA_KAGENT_OPENAI_REASONING_EFFORT       ◀ delta
  plugins  = [ ..stock plugins.. ]
  plugins += [ AppaPluginKagent(APPA_RUNTIME_URL) ]  ◀ delta
  code_executor, memory persist ─▶ wrapped: each
      crosses the tool gate as a synthetic call    ◀ delta
  tools   += [ execute_remedy_plan over
               $APPA_RUNTIME_URL/mcp · 300 s ]    ◀ delta
  KAgentApp(root_agent, card, url, name,
            plugins=plugins).build()   ─▶ serve A2A
```

The entrypoint replays the stock startup and appends one plugin to the list ADK already accepts. That one registration covers the root agent, every sub-agent, and every tool. The Go image does the same through the Go ADK plugin API, and its runtime main restores two python-side shapes the Go ADK lacks: it lands the lineage headers in session state, so a delegated child starts as a child, and it keeps a reviewed remedy out of the task history until the person rules, so the dashboard shows the approval card. One model field rides along: `APPA_KAGENT_OPENAI_REASONING_EFFORT` fills the OpenAI model's `reasoning_effort` when the ModelConfig leaves it unset — the v1alpha2 ModelConfig cannot say `none`, and the gpt-5.6 models the demo runs on require it for function tools on chat completions. A value the ModelConfig sets wins.

### Callback-to-hook mapping

The runtime hook vocabulary is the eight `HookEvent` variants of `appa-runtime-api`. The plugin maps each gated ADK callback onto exactly one event. Callbacks with no event either pass through or hold as liveness gates.

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
│           ═▶ [3] ToolCall{spawn:false, ruling?}
│              ◀ AllowCall | DenyCall{review} | Refuse
│              ◀ PassControl — execute_remedy_plan only
│              review: per offer whose plan consults a
│              hitl authority; ruling: the person's
│              approve | deny on the resumed reserved call
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
- An agent called as a tool crosses the same gate as any tool call. The parent side substitutes its return.

### Every gated call runs under one contract

The policy produces the contract for a tool call in one of two ways. It is either a static declaration or a registered annotator that answers per call. The consult happens inside the tool gate, on the runtime side, and kagent never sees it. A wildcard annotator covers the tools the policy never names. That posture fits a kagent fleet, where CRD-declared toolsets produce a long tail of tools.

### Remedy plans stay executable

A block is not a dead end. The blocking feedback quotes an offer id, and the agent executes the offered plan through `execute_remedy_plan` — the reserved tool `appa-runtime` itself serves. The images inject it at agent construction, beside `AppaPluginKagent`, so every declarative agent carries it with zero agent changes. Both images' MCP clients wait 300 s per call: a remedy execution holds `execute_remedy_plan` open for as long as its plan runs — a parked authority consult, a slow sanitizer — and ADK's 5 s default would fail it at the client.

```text
tool call blocked ─▶ the feedback quotes an offer id
        │
        ▼
execute_remedy_plan(offer_id)
        │   the reserved appa-runtime tool, injected
        │   at agent construction beside the plugin
        ▼
ToolCall hook  ◀ PassControl — vouched, the call
        │      passes through to appa-runtime
        │      (a reviewed offer first asks the person:
        │       the human-review flow below)
        ▼
the offered plan executes:
   Authorize · Accept · Sanitize · Derive
a Redispatch plan names a tool instead — the agent
calls it itself, through the normal gate
```

- Human review rides the stock kagent approval flow, for exactly the remedies whose plan names a human authority. The blocking decision carries the review the person reads, the plugin raises kagent's confirmation for that one `execute_remedy_plan` call, the run suspends, and the person's Approve or Reject returns to `appa-runtime` as the authority's ruling — never through the model; the model learns only that the reviewer has been asked. Approve authorizes that one execution; Reject is the authority's denial and retires the offer; kagent has no cancel, so an abandoned task leaves the offer standing. Over A2A the confirmation carries the full review; the kagent dashboard shows the tool call and its arguments. The Go image carries the same channel: adk-go hands its plugin the tool context, so the reviewed call raises the confirmation and the resumed call returns the ruling the same way.
- Every other remedy — a narrowing, a sanitizer, a human-less authority, a redispatch — the agent executes itself: no confirmation gate sits on `execute_remedy_plan`, and the agent chooses among the offers under its instruction and the chat. The demo agent's instruction takes the sanitized result when one is offered, otherwise accepts the change, follows a steer from the chat, and reports the remedy it took.
- People out of band need no kagent surface. A URL authority such as the demo's `change-board` on `rollback_deployment` parks the consult inside the `execute_remedy_plan` call until the ruling arrives on the authority's own channel or its window closes. Approve runs the rollback, Deny retires the offer, and no answer grants nothing — the offer stands.

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

- Every remedy call crosses the same hook gate as any tool call.

### A delegated child starts at its parent's label

kagent delegates by calling another Agent as a tool. On the wire that is a spawn: the runtime prepares a fork seeded with the parent's current label — trust and audience, both inherited — and the child pod opens it through kagent's lineage headers, so its calls land in the child trajectory, in the same log as its parent. Inheritance is why delegation cures nothing on its own: a child would be blocked exactly where the parent is. What the child then does narrows the child, not the parent. Only the value that comes back meets the parent's gate, carrying the child's label. A raw value that narrows nothing crosses unchanged. A raw value that would narrow the parent is withheld there with the parent's own offers: accept the narrowing and it crosses, narrowing the parent as if it had done the read itself; take a sanitizer and only the derivation crosses, leaving the parent as it was. A child that returns nothing, or a return the runtime withholds, changes nothing. That is the productive use of a child — quarantine untrusted work in a disposable trajectory and bring back only a clean derivation.

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

Four hooks carry it: the spawn on the parent's tool call, `ChildStart` when the child pod enters, `SpawnResult` when the value returns to the parent's gate, and the child's own `TurnEnd`. The demo's `log-analyst` is that child; `confined_child_return` lets the runtime withhold its return or substitute a bound return sanitizer's derivation, and the delegation case in both matrices asserts the injected instruction never reaches the operator through it.

Delegation is off by default. The wildcard entry covers every ordinary call the policy does not write, but on kagent it covers no spawn: a child trajectory is not something a per-call annotation can stand for. An agent the policy never names is denied at the spawn, with the reason as the model's feedback, and the agent never runs. The demo shows both sides: `log-analyst` is named and runs as a child; `release-manager` is listed by the same parent, named by no contract, and every delegation to it is denied.

### Fail-closed rules

1. An unreachable `/hook`, or an answer outside the contract, blocks the gated action.
2. A config field the python entrypoint does not support refuses to start. The stock parser ignores unknown fields, and that entrypoint does not.
3. The model and emission callbacks feed no event, but they still hold when the `/hook` channel is down.
4. When the pinned ADK has no error-turn callback, `appa-runtime` recovery closes the turn at the next admitted event.
5. A delegation to an agent the policy does not name is denied, wildcard or not. On kagent an agent runs as a child only under a contract that names it, so delegation is off until the policy names the agent. The quickstart's packaged policy names none.

### Scope

Covered: declarative agents on both runtimes. Not covered: BYO agents and the kagent sandbox kinds.

### Configure the fleet

The demo chart also installs an `appa-guide` agent: the OpenAPPA guide skill attached through kagent's git-ref skills, the kagent tool server's k8s tools, and the shared runtime as its own gate. Open its chat and say `init`.

The skill is one `SKILL.md` that routes by host to a reference file. On kagent it reads the policy ConfigMap, inventories every `RemoteMCPServer` from `status.discoveredTools` and every `Agent`'s declared tools, proposes contracts in plain English, and waits for chat approval. The apply then writes the ConfigMap through `k8s_apply_manifest` — the fleet policy puts that call behind `attention = ["human-approval"]`, so the kagent Approve/Reject card is the human sign-off — waits for the mounted policy to sync, and reloads the runtime. Any host with the same tools can run the same skill; the agent is only packaging.

### Try it

The demo is a Helm chart, [integrations/kagent/demo/chart](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/kagent/demo/chart): a gated `cluster-ops` agent with a delegated `log-analyst`, the shared `appa-runtime` with its relay and mock externals in one pod, the demo tools, and every demo case pre-seeded as a real chat in the kagent dashboard — an ordinary read, the exfiltration ask that leaks nothing, the agent taking a sanitized remedy on its own, the chat steering it to accept the change or to decline, a forged offer, the on-call approval, the annotator, the release window in and out of window, the remote change board approving, denying and staying silent, delegation, a delegation the policy never names, and gated ingress. It installs into any cluster running kagent 0.9.12 with `controller.agentImage` set to `appa-kagent-quickstart`; the model key is the one input, in a value or pasted in the dashboard afterwards — the Secret is named after the ModelConfig, so the dashboard's Models → Edit flow supplies it. The default image references name this repository's release tags; the chart README shows how to build the images from source and point the image values at your own registry.

The chart also runs both agents as `cluster-ops-go` and `log-analyst-go` on kagent's Go runtime, with the Go image under the name kagent derives for it. Every case runs the same on either cell.

Two matrices verify the install. [e2e/ui](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/kagent/e2e/ui) drives seventeen conversations — the sixteen seeded cases and the on-call rejection — through the kagent dashboard in headless Chromium with a real model; [e2e/a2a](https://github.com/archestra-ai/OpenAPPA/tree/main/integrations/kagent/e2e/a2a) runs the same seventeen over the A2A protocol alone, answers the human-review confirmation with the data part the dashboard sends, and plays the change-board member on the mock's side channel. Both pass 16/16 on the Helm-installed stack, on the python cell and on the Go cell. The matrix spans kagent version, runtime plugin and driver; only kagent v0.9.12 runs today.

## Implementation plan

The [kagent implementation plan](https://github.com/archestra-ai/OpenAPPA/blob/main/integrations/kagent/IMPLEMENTATION.md) carries the rest. It covers source baselines, the target matrix, per-version mapping tables, both delivery lanes, the quickstart option, remedy-plan execution with the human-review channel, the wire obligations a driver keeps, the demo chart, and the verification matrix.
